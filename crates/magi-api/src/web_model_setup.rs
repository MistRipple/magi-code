//! 第一方 GPT Web 配置命令。复用普通 Turn / canonical、页面驱动和通道服务，
//! 不借用正在配置的 GPT Web 模型，也不把任何凭据放进对话。
use std::sync::Arc;

use magi_app_server_protocol::generated::WebModelSetupStatus;
use magi_core::{SessionId, UtcMillis};
use magi_plugin_system::{
    PluginCommandExecutor, PluginCommandInvocation, PluginCommandResult, RunCancellation,
};
use serde_json::json;

use crate::web_model_ops::{WEB_MODEL_HOME_TAB_ID, ensure_app_browser_session};
use crate::{ApiError, ApiState};

pub const SETUP_COMMAND_ID: &str = "plugin/openai.chatgpt-web/setup";
const SETUP_SECTION: &str = "webModelSetup";

pub struct WebModelSetupCommandExecutor {
    state: ApiState,
    next: Arc<dyn PluginCommandExecutor>,
    runtime: tokio::runtime::Handle,
}

impl WebModelSetupCommandExecutor {
    pub fn new(state: ApiState, next: Arc<dyn PluginCommandExecutor>) -> Self {
        Self {
            state,
            next,
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl PluginCommandExecutor for WebModelSetupCommandExecutor {
    fn invoke(
        &self,
        invocation: PluginCommandInvocation,
        cancellation: &RunCancellation,
    ) -> Result<PluginCommandResult, String> {
        if invocation.command_id != SETUP_COMMAND_ID {
            return self.next.invoke(invocation, cancellation);
        }
        let session = SessionId::new(invocation.session_id);
        if !matches!(
            invocation.input.trim(),
            "" | "start" | "continue" | "confirm_connector" | "cancel"
        ) {
            return Err("配置命令不接受凭据，请使用配置卡片的安全输入框".into());
        }
        // 每个 invocation 都重新验证插件，停用 / 撤权不能继续配置。
        if !self.state.web_model_plugin_active() {
            return Err("请先到设置 → 插件安装并激活 GPT Web，再开始配置".into());
        }
        let digest = {
            let manager = self
                .state
                .plugin_manager
                .lock()
                .map_err(|_| "插件状态不可用")?;
            let package = manager
                .package(magi_web_model::WEB_MODEL_PLUGIN_ID)
                .map_err(|_| "GPT Web 插件尚未安装")?;
            if !package
                .manifest()
                .contributions
                .commands
                .iter()
                .any(|command| command.id == "setup")
            {
                return Err("请先升级 GPT Web 插件后再开始配置".into());
            }
            package.digest().to_string()
        };
        let _lease = magi_plugin_system::PluginRuntimeLease::acquire(
            self.state.plugin_manager.clone(),
            magi_web_model::WEB_MODEL_PLUGIN_ID,
            &digest,
            magi_plugin_system::APPLICATION_SCOPE,
            cancellation,
        )
        .map_err(|_| "GPT Web 插件正在停用或升级")?;
        // 已发出的页面写入等待真实结算；只在阶段边界响应取消，不能丢弃 future 后重放。
        let result = self
            .runtime
            .block_on(crate::web_model_driver::with_web_setup_turn(
                invocation.turn_id,
                self.state
                    .advance_web_model_setup(&session, invocation.input.trim(), cancellation),
            ))
            .map_err(|_| "配置已暂停或不可用，请检查配置卡片后继续")?;
        Ok(PluginCommandResult {
            content: setup_message(&result).to_string(),
        })
    }
}

pub(crate) fn setup_message(status: &WebModelSetupStatus) -> &'static str {
    match status.stage.as_str() {
        "login" => {
            "请在右侧 GPT Web 页面完成 ChatGPT 登录，然后点击「我已完成，继续」。登录、验证码和双重验证由你操作。"
        }
        "tunnel"
            if matches!(
                status.reason.as_str(),
                "creation_started" | "creation_unconfirmed"
            ) =>
        {
            "Tunnel 创建结果尚未确认。请检查右侧平台页；继续时 Magi 只回读，不会重复创建。也可将已创建的 Tunnel id 填入安全表单。"
        }
        "tunnel" => {
            "已打开 OpenAI Platform。请完成登录或选择有权限的组织，然后点击「我已完成，继续」。Magi 将检查并创建专用 Tunnel。"
        }
        "credentials" => {
            "需要一个仅含 Tunnels Read + Use 权限的运行时 API Key。请在右侧平台页创建，再粘贴到下方安全输入框；不要把密钥发送到聊天中。"
        }
        "connecting" => {
            "通道组件正在准备或连接。就绪后 Magi 会自动继续；连接失败时请根据配置卡片的提示处理后重试。"
        }
        "connector"
            if matches!(
                status.reason.as_str(),
                "connector_write_started" | "connector_unconfirmed"
            ) =>
        {
            "连接器配置结果尚未确认。请在 ChatGPT 的设置 → 应用中检查或完成 Magi 连接器配置，再点击继续。Magi 只回读结果，不会重复提交。"
        }
        "connector" => {
            "通道已连接。下一步会在 ChatGPT 账号中创建或刷新名为 Magi 的连接器。请点击「允许配置连接器并继续」。项目工具仍按现有权限与逐次审批执行。"
        }
        "complete" => {
            "GPT Web 配置已完成：通道运行正常，Magi 连接器已回读确认。实际项目工具调用会在工作区会话中执行，并遵循 Magi 的权限和审批。"
        }
        "cancelled" => "已结束本次配置，已保存的 Tunnel 和密钥保留。之后可以重新开始。",
        "busy" => {
            "GPT Web 页面正在被其他会话或配置任务使用。请先处理占用，再继续配置；Magi 不会中断现有网页对话。"
        }
        "failed" => {
            "当前步骤未能确认成功。请查看配置卡片提示，在页面中处理后继续；Magi 会重新检查实际状态。"
        }
        _ => "正在检查 GPT Web 配置。",
    }
}

impl ApiState {
    pub(crate) fn web_model_setup_status(
        &self,
        session: &SessionId,
    ) -> Option<WebModelSetupStatus> {
        serde_json::from_value::<WebModelSetupStatus>(
            self.settings_store.get_section(SETUP_SECTION),
        )
        .ok()
        .filter(|record| record.session_id == session.as_str())
    }

    fn save_setup_stage(
        &self,
        session: &SessionId,
        stage: &str,
        reason: &str,
    ) -> Result<WebModelSetupStatus, ApiError> {
        let record = WebModelSetupStatus {
            session_id: session.to_string(),
            stage: stage.into(),
            reason: reason.into(),
            updated_at: UtcMillis::now().0 as i64,
        };
        self.settings_store
            .set_section(SETUP_SECTION, json!(record))
            .map_err(crate::errors::settings_persistence_error)?;
        Ok(record)
    }

    /// 恢复只恢复维护占用，不重放页面创建或账号写操作。
    pub fn restore_web_model_setup_reservation(&self) {
        let Ok(record) = serde_json::from_value::<WebModelSetupStatus>(
            self.settings_store.get_section(SETUP_SECTION),
        ) else {
            return;
        };
        if !matches!(record.stage.as_str(), "complete" | "cancelled" | "busy")
            && self.web_model_plugin_active()
            && self
                .session_store
                .session(&SessionId::new(&record.session_id))
                .is_some()
            && let Some(slots) = self.web_model.bindings()
        {
            let _ = slots.reserve_maintenance(&record.session_id);
        }
    }

    pub(crate) async fn advance_web_model_setup(
        &self,
        session: &SessionId,
        action: &str,
        cancellation: &RunCancellation,
    ) -> Result<WebModelSetupStatus, ApiError> {
        let _serial = self.web_model.setup_lock.lock().await;
        if !self.web_model_plugin_active() {
            return Err(ApiError::Forbidden(
                "请先到设置 → 插件安装并激活 GPT Web".into(),
            ));
        }
        if self.session_store.session(session).is_none() {
            return Err(ApiError::NotFound("会话不存在".into()));
        }
        let slots = self
            .web_model
            .bindings()
            .ok_or_else(|| ApiError::Conflict("Desktop 浏览器尚未就绪".into()))?;
        if action == "cancel" {
            if slots
                .maintenance_owner()
                .as_deref()
                .is_some_and(|owner| owner != session.as_str())
            {
                return Err(ApiError::Conflict(
                    "另一会话正在配置 GPT Web，请回到该会话结束配置".into(),
                ));
            }
            let reason = self
                .web_model_setup_status(session)
                .map(|record| pending_write_reason(&record).to_string())
                .unwrap_or_default();
            let record = self.save_setup_stage(session, "cancelled", &reason)?;
            slots.release_maintenance(session.as_str());
            return Ok(record);
        }
        let existing = self.web_model_setup_status(session).or_else(|| {
            // 结束后从另一会话重开仍继承未确认写入，不能靠换会话重放。
            serde_json::from_value::<WebModelSetupStatus>(
                self.settings_store.get_section(SETUP_SECTION),
            )
            .ok()
            .filter(|record| !pending_write_reason(record).is_empty())
        });
        if slots.reserve_maintenance(session.as_str()).is_err() {
            // 不覆盖另一项正在等待的配置记录。
            return Ok(WebModelSetupStatus {
                session_id: session.to_string(),
                stage: "busy".into(),
                reason: "web_session_busy".into(),
                updated_at: UtcMillis::now().0 as i64,
            });
        }
        if matches!(action, "start" | "continue" | "confirm_connector") {
            let mut authority = self
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned");
            let _ = authority
                .release_user_control(&magi_core::BrowserTabId::new(WEB_MODEL_HOME_TAB_ID));
        }
        let result = self
            .advance_setup_inner(session, existing.as_ref(), action, cancellation)
            .await;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                // 错误只保留固定公开状态，不把底层页面 / 凭据诊断写入配置记录。
                tracing::warn!(session_id = %session, "GPT Web 配置步骤未确认成功");
                let _ = error;
                let checkpoint = self.web_model_setup_status(session);
                if let Some(checkpoint) =
                    checkpoint.filter(|record| !pending_write_reason(record).is_empty())
                {
                    return Ok(checkpoint);
                }
                self.save_setup_stage(session, "failed", "step_unconfirmed")
            }
        }
    }

    async fn advance_setup_inner(
        &self,
        session: &SessionId,
        previous: Option<&WebModelSetupStatus>,
        action: &str,
        cancellation: &RunCancellation,
    ) -> Result<WebModelSetupStatus, ApiError> {
        let check = || {
            if cancellation.is_cancelled() || !self.web_model_plugin_active() {
                Err(ApiError::Conflict("配置已暂停".into()))
            } else {
                Ok(())
            }
        };
        check()?;
        ensure_app_browser_session(self)?;
        let driver = self
            .web_model
            .driver()
            .ok_or_else(|| ApiError::Conflict("GPT Web 页面驱动尚未就绪".into()))?;
        let map_error = crate::web_model_ops::web_model_api_error;
        let stage = previous
            .map(|record| record.stage.as_str())
            .unwrap_or("login");
        if matches!(
            stage,
            "login" | "cancelled" | "complete" | "failed" | "busy"
        ) {
            driver
                .open_page(
                    WEB_MODEL_HOME_TAB_ID,
                    &magi_web_model::chatgpt_web_home_url(),
                )
                .await
                .map_err(map_error)?;
            let page = driver
                .turn_state(WEB_MODEL_HOME_TAB_ID)
                .await
                .map_err(map_error)?;
            if page.login_state != magi_web_model::LoginState::SignedIn
                || page.blocked
                || !page.composer_found
            {
                let reason = previous
                    .map(pending_write_reason)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or(if page.blocked {
                        "chatgpt_verification_required"
                    } else {
                        "chatgpt_login_required"
                    });
                return self.save_setup_stage(session, "login", reason);
            }
        }
        let mut config = self.web_model.configured().unwrap_or_default();
        if config.tunnel_id.trim().is_empty() {
            driver
                .open_page(
                    WEB_MODEL_HOME_TAB_ID,
                    &magi_web_model::openai_platform_tunnels_url(),
                )
                .await
                .map_err(map_error)?;
            check()?;
            // 在投递创建动作之前持久化不确定边界，重启 / 取消后只能回读，不能重放创建。
            let inspect_only = previous.is_some_and(|record| {
                matches!(
                    record.reason.as_str(),
                    "creation_started" | "creation_unconfirmed"
                )
            });
            self.save_setup_stage(session, "tunnel", "creation_started")?;
            let value = driver
                .platform_setup(if inspect_only {
                    "inspect_tunnel"
                } else {
                    "create_tunnel"
                })
                .await
                .map_err(map_error)?;
            let Some(id) = value
                .get("tunnel_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| valid_tunnel_id(id))
            else {
                let reason = value
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("manual_required");
                return self.save_setup_stage(
                    session,
                    "tunnel",
                    if inspect_only {
                        "creation_unconfirmed"
                    } else {
                        reason
                    },
                );
            };
            config.tunnel_id = id.into();
            self.settings_store
                .set_section(crate::WEB_MODEL_TUNNEL_SECTION, json!(config))
                .map_err(crate::errors::settings_persistence_error)?;
            self.web_model
                .clone()
                .configure(self, Some(config.clone()), None)
                .await
                .map_err(ApiError::InvalidInput)?;
        }
        check()?;
        let status = self.web_model.status();
        if !status.has_api_key {
            driver
                .open_page(
                    WEB_MODEL_HOME_TAB_ID,
                    &magi_web_model::openai_platform_api_keys_url(),
                )
                .await
                .map_err(map_error)?;
            return self.save_setup_stage(session, "credentials", "runtime_key_required");
        }
        if !status.ready {
            // 保存过配置但未启动时复用唯一通道配置入口；准备中不重复重启。
            if !matches!(
                status.code.as_str(),
                "client_installing" | "runtime_not_ready"
            ) {
                self.web_model
                    .clone()
                    .configure(self, Some(config), None)
                    .await
                    .map_err(ApiError::InvalidInput)?;
            }
            let reason = previous
                .map(pending_write_reason)
                .filter(|reason| !reason.is_empty())
                .unwrap_or(&status.code);
            return self.save_setup_stage(session, "connecting", reason);
        }
        self.advance_setup_connector(
            session,
            previous,
            action,
            &status.tunnel_id,
            driver.as_ref(),
            cancellation,
        )
        .await
    }

    async fn advance_setup_connector(
        &self,
        session: &SessionId,
        previous: Option<&WebModelSetupStatus>,
        action: &str,
        tunnel_id: &str,
        driver: &dyn magi_web_model::WebModelPageDriver,
        cancellation: &RunCancellation,
    ) -> Result<WebModelSetupStatus, ApiError> {
        let check = || {
            if cancellation.is_cancelled() || !self.web_model_plugin_active() {
                Err(ApiError::Conflict("配置已暂停".into()))
            } else {
                Ok(())
            }
        };
        let map_error = crate::web_model_ops::web_model_api_error;
        check()?;
        // 写入曾经开始时只回读。即使用户重启或结束后重开，也不能把未确认当成未执行。
        if previous.is_some_and(|record| {
            matches!(
                record.reason.as_str(),
                "connector_write_started" | "connector_unconfirmed"
            )
        }) {
            let outcome = driver
                .connector_status(WEB_MODEL_HOME_TAB_ID, "Magi")
                .await
                .map_err(map_error)?;
            check()?;
            if outcome.supported && outcome.exists && outcome.enabled {
                let record = self.save_setup_stage(session, "complete", "")?;
                self.web_model
                    .bindings()
                    .unwrap()
                    .release_maintenance(session.as_str());
                return Ok(record);
            }
            return self.save_setup_stage(session, "connector", "connector_unconfirmed");
        }
        if action != "confirm_connector" {
            return self.save_setup_stage(session, "connector", "confirmation_required");
        }
        // 显式确认只在通道已就绪时生效，调用现有页面驱动并回读结果。
        if previous.is_none_or(|record| record.stage != "connector") {
            return self.save_setup_stage(session, "connector", "confirmation_required");
        }
        check()?;
        self.save_setup_stage(session, "connector", "connector_write_started")?;
        let outcome = driver
            .configure_connector(WEB_MODEL_HOME_TAB_ID, "Magi", tunnel_id)
            .await
            .map_err(map_error)?;
        check()?;
        if !outcome.configured || !outcome.confirmed_enabled {
            return self.save_setup_stage(session, "connector", "connector_unconfirmed");
        }
        let record = self.save_setup_stage(session, "complete", "")?;
        self.web_model
            .bindings()
            .unwrap()
            .release_maintenance(session.as_str());
        Ok(record)
    }

    pub(crate) async fn cancel_web_model_setup_for(&self, session: Option<&SessionId>) {
        let _serial = self.web_model.setup_lock.lock().await;
        let Some(slots) = self.web_model.bindings() else {
            return;
        };
        let Some(owner) = slots.maintenance_owner() else {
            return;
        };
        if session.is_some_and(|session| session.as_str() != owner) {
            return;
        }
        let session = SessionId::new(&owner);
        let reason = self
            .web_model_setup_status(&session)
            .map(|record| pending_write_reason(&record).to_string())
            .unwrap_or_default();
        let _ = self.save_setup_stage(&session, "cancelled", &reason);
        slots.release_maintenance(&owner);
    }

    /// 密钥只通过可信 Desktop 的安全表单进入这里；不通过 Turn 或命令参数传递。
    pub(crate) async fn save_setup_credentials(
        &self,
        session: &SessionId,
        tunnel_id: Option<String>,
        api_key: String,
    ) -> Result<(), ApiError> {
        let _serial = self.web_model.setup_lock.lock().await;
        if !self.web_model_plugin_active() {
            return Err(ApiError::Forbidden("请先安装并激活 GPT Web 插件".into()));
        }
        let digest = self
            .plugin_manager
            .lock()
            .map_err(|_| ApiError::Forbidden("插件状态不可用".into()))?
            .package(magi_web_model::WEB_MODEL_PLUGIN_ID)
            .map_err(|_| ApiError::Forbidden("GPT Web 插件未安装".into()))?
            .digest()
            .to_string();
        let cancellation = RunCancellation::default();
        let _lease = magi_plugin_system::PluginRuntimeLease::acquire(
            self.plugin_manager.clone(),
            magi_web_model::WEB_MODEL_PLUGIN_ID,
            &digest,
            magi_plugin_system::APPLICATION_SCOPE,
            &cancellation,
        )
        .map_err(|_| ApiError::Conflict("GPT Web 插件正在停用或升级".into()))?;
        if self.session_store.session(session).is_none() {
            return Err(ApiError::NotFound("会话不存在".into()));
        }
        if self
            .web_model
            .bindings()
            .and_then(|slots| slots.maintenance_owner())
            .as_deref()
            != Some(session.as_str())
        {
            return Err(ApiError::Conflict("当前会话没有配置页面控制权".into()));
        }
        let record = self
            .web_model_setup_status(session)
            .ok_or_else(|| ApiError::Conflict("配置任务不存在".into()))?;
        if !matches!(record.stage.as_str(), "credentials" | "tunnel") {
            return Err(ApiError::Conflict("当前步骤不接受凭据".into()));
        }
        let mut config = self.web_model.configured().unwrap_or_default();
        if let Some(id) = tunnel_id.filter(|id| !id.trim().is_empty()) {
            if !valid_tunnel_id(id.trim()) {
                return Err(ApiError::InvalidInput("Tunnel id 格式不正确".into()));
            }
            config.tunnel_id = id.trim().into();
        }
        if config.tunnel_id.is_empty() {
            return Err(ApiError::InvalidInput("请先填写 Tunnel id".into()));
        }
        self.settings_store
            .set_section(crate::WEB_MODEL_TUNNEL_SECTION, json!(config))
            .map_err(crate::errors::settings_persistence_error)?;
        self.web_model
            .clone()
            .configure(
                self,
                Some(config),
                (!api_key.trim().is_empty()).then_some(api_key),
            )
            .await
            .map_err(ApiError::InvalidInput)?;
        self.save_setup_stage(session, "connecting", "")?;
        Ok(())
    }
}

fn valid_tunnel_id(id: &str) -> bool {
    magi_web_model::tunnel::valid_tunnel_id(id)
}

fn pending_write_reason(record: &WebModelSetupStatus) -> &str {
    match record.reason.as_str() {
        "creation_started" | "creation_unconfirmed" => "creation_unconfirmed",
        "connector_write_started" | "connector_unconfirmed" => "connector_unconfirmed",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_web_model::{DriverFuture, WebModelError, WebModelErrorCode, WebModelPageDriver};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct SetupDriver {
        logged_in: AtomicBool,
        lose_creation_result: AtomicBool,
        connector_enabled: AtomicBool,
        connector_writes: AtomicUsize,
        platform_actions: Mutex<Vec<String>>,
        pages: Mutex<Vec<String>>,
    }

    impl WebModelPageDriver for SetupDriver {
        fn open_page<'a>(&'a self, _: &'a str, url: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async move {
                self.pages.lock().unwrap().push(url.into());
                Ok(())
            })
        }
        fn resume_page<'a>(&'a self, _: &'a str) -> DriverFuture<'a, bool> {
            Box::pin(async { Ok(self.logged_in.load(Ordering::SeqCst)) })
        }
        fn platform_setup<'a>(&'a self, action: &'a str) -> DriverFuture<'a, serde_json::Value> {
            Box::pin(async move {
                self.platform_actions.lock().unwrap().push(action.into());
                if self.lose_creation_result.load(Ordering::SeqCst) {
                    if action == "create_tunnel" {
                        return Err(WebModelError::new(
                            WebModelErrorCode::WebTunnelUnavailable,
                            "SECRET_DIAGNOSTIC",
                        ));
                    }
                    return Ok(json!({"status":"missing"}));
                }
                Ok(json!({"status":"ready", "tunnel_id":"tunnel_0123456789abcdef0123456789abcdef"}))
            })
        }
        fn connector_status<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> DriverFuture<'a, magi_web_model::ConnectorStatus> {
            Box::pin(async {
                Ok(magi_web_model::ConnectorStatus {
                    supported: true,
                    exists: self.connector_enabled.load(Ordering::SeqCst),
                    enabled: self.connector_enabled.load(Ordering::SeqCst),
                    tool_count: None,
                    reason: None,
                })
            })
        }
        fn configure_connector<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a str,
        ) -> DriverFuture<'a, magi_web_model::ConnectorConfigOutcome> {
            Box::pin(async {
                self.connector_writes.fetch_add(1, Ordering::SeqCst);
                Err(WebModelError::new(
                    WebModelErrorCode::WebTunnelUnavailable,
                    "SECRET_DIAGNOSTIC",
                ))
            })
        }
        fn open_temporary_chat<'a>(&'a self, _: &'a str) -> DriverFuture<'a, ()> {
            unreachable!()
        }
        fn close_page<'a>(&'a self, _: &'a str) -> DriverFuture<'a, ()> {
            unreachable!()
        }
        fn write_text<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> DriverFuture<'a, magi_web_model::WriteOutcome> {
            unreachable!()
        }
        fn submit<'a>(&'a self, _: &'a str) -> DriverFuture<'a, magi_web_model::SubmitOutcome> {
            unreachable!()
        }
        fn turn_state<'a>(&'a self, _: &'a str) -> DriverFuture<'a, magi_web_model::TurnState> {
            Box::pin(async {
                Ok(magi_web_model::TurnState {
                    login_state: if self.logged_in.load(Ordering::SeqCst) {
                        magi_web_model::LoginState::SignedIn
                    } else {
                        magi_web_model::LoginState::SignedOut
                    },
                    blocked: false,
                    generating: false,
                    composer_found: true,
                    user_message_count: 0,
                    assistant_message_count: 0,
                    assistant_text: String::new(),
                    thinking_text: String::new(),
                    question: None,
                    last_message_role: None,
                    last_message_text: None,
                })
            })
        }
    }

    fn fixture() -> (ApiState, SessionId, Arc<SetupDriver>, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            "web-setup-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
            Arc::new(magi_session_store::SessionStore::default()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        )
        .with_runtime_persistence(Arc::new(crate::state::RuntimeStatePersistence::new(
            root.path(),
            root.path().join("workspaces.json"),
            root.path().join("knowledge.json"),
        )));
        let session = SessionId::new("setup-session");
        state
            .session_store
            .create_session_for_workspace(session.clone(), "Setup", None)
            .unwrap();
        state
            .web_model
            .set_bindings(Arc::new(magi_web_model::WebSlotTable::new()));
        let driver = Arc::new(SetupDriver::default());
        state.web_model.set_driver(driver.clone());
        (state, session, driver, root)
    }

    #[tokio::test]
    async fn setup_requires_plugin_and_resumes_after_user_login() {
        let (state, session, driver, _root) = fixture();
        let cancel = RunCancellation::default();
        assert!(matches!(
            state
                .advance_web_model_setup(&session, "start", &cancel)
                .await,
            Err(ApiError::Forbidden(_))
        ));
        assert!(driver.pages.lock().unwrap().is_empty());
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        assert_eq!(
            state
                .advance_web_model_setup(&session, "start", &cancel)
                .await
                .unwrap()
                .stage,
            "login"
        );
        driver.logged_in.store(true, Ordering::SeqCst);
        assert_eq!(
            state
                .advance_web_model_setup(&session, "continue", &cancel)
                .await
                .unwrap()
                .stage,
            "credentials"
        );
        assert_eq!(
            state.web_model.configured().unwrap().tunnel_id,
            "tunnel_0123456789abcdef0123456789abcdef"
        );
        assert_eq!(*driver.platform_actions.lock().unwrap(), ["create_tunnel"]);
        assert!(
            !state
                .settings_store
                .get_section(SETUP_SECTION)
                .to_string()
                .contains("SECRET")
        );
    }

    #[tokio::test]
    async fn unconfirmed_tunnel_is_never_recreated_even_after_cancel_and_new_session() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        driver.logged_in.store(true, Ordering::SeqCst);
        driver.lose_creation_result.store(true, Ordering::SeqCst);
        let cancel = RunCancellation::default();
        state
            .save_setup_stage(
                &session,
                "tunnel",
                "platform_login_or_organization_required",
            )
            .unwrap();
        assert_eq!(
            state
                .advance_web_model_setup(&session, "continue", &cancel)
                .await
                .unwrap()
                .reason,
            "creation_started"
        );
        for _ in 0..2 {
            assert_eq!(
                state
                    .advance_web_model_setup(&session, "continue", &cancel)
                    .await
                    .unwrap()
                    .reason,
                "creation_unconfirmed"
            );
        }
        state
            .advance_web_model_setup(&session, "cancel", &cancel)
            .await
            .unwrap();
        let other = SessionId::new("other-session");
        state
            .session_store
            .create_session_for_workspace(other.clone(), "Other", None)
            .unwrap();
        state
            .advance_web_model_setup(&other, "start", &cancel)
            .await
            .unwrap();
        state
            .advance_web_model_setup(&other, "continue", &cancel)
            .await
            .unwrap();
        assert_eq!(
            driver
                .platform_actions
                .lock()
                .unwrap()
                .iter()
                .filter(|action| action.as_str() == "create_tunnel")
                .count(),
            1
        );
        assert!(
            !state
                .settings_store
                .get_section(SETUP_SECTION)
                .to_string()
                .contains("SECRET")
        );
    }

    #[tokio::test]
    async fn connector_requires_consent_and_recovers_unconfirmed_write_by_reading() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        let slots = state.web_model.bindings().unwrap();
        slots.reserve_maintenance(session.as_str()).unwrap();
        let cancel = RunCancellation::default();
        let confirmation = state
            .advance_setup_connector(
                &session,
                None,
                "confirm_connector",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(confirmation.reason, "confirmation_required");
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 0);
        assert!(
            state
                .advance_setup_connector(
                    &session,
                    Some(&confirmation),
                    "confirm_connector",
                    "tunnel_0123456789abcdef0123456789abcdef",
                    driver.as_ref(),
                    &cancel
                )
                .await
                .is_err()
        );
        let checkpoint = state.web_model_setup_status(&session).unwrap();
        assert_eq!(checkpoint.reason, "connector_write_started");
        let waiting = state
            .advance_setup_connector(
                &session,
                Some(&checkpoint),
                "confirm_connector",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(waiting.reason, "connector_unconfirmed");
        driver.connector_enabled.store(true, Ordering::SeqCst);
        let done = state
            .advance_setup_connector(
                &session,
                Some(&waiting),
                "continue",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(done.stage, "complete");
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 1);
        assert!(slots.maintenance_owner().is_none());
    }

    #[tokio::test]
    async fn occupied_chat_is_preserved_and_credentials_require_current_setup_ownership() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        let slots = state.web_model.bindings().unwrap();
        let owner = magi_web_model::WebSlotOwner::new("chat", "project");
        slots
            .claim(
                owner.clone(),
                magi_web_model::WebConversationBinding::temporary(),
                "page",
            )
            .unwrap();
        let cancel = RunCancellation::default();
        assert_eq!(
            state
                .advance_web_model_setup(&session, "start", &cancel)
                .await
                .unwrap()
                .stage,
            "busy"
        );
        assert_eq!(slots.owner(), Some(owner));
        assert!(driver.pages.lock().unwrap().is_empty());
        assert!(
            state
                .save_setup_credentials(
                    &session,
                    Some("tunnel_0123456789abcdef0123456789abcdef".into()),
                    "secret".into()
                )
                .await
                .is_err()
        );
        assert!(
            state
                .settings_store
                .get_section(crate::WEB_MODEL_TUNNEL_SECTION)
                .is_null()
        );
    }

    #[tokio::test]
    async fn restore_only_reserves_the_page_and_never_replays_pending_writes() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "tunnel", "creation_started")
            .unwrap();
        state.restore_web_model_setup_reservation();
        assert_eq!(
            state
                .web_model
                .bindings()
                .unwrap()
                .maintenance_owner()
                .as_deref(),
            Some(session.as_str())
        );
        assert!(driver.platform_actions.lock().unwrap().is_empty());
        assert!(driver.pages.lock().unwrap().is_empty());
        state.cancel_web_model_setup_for(Some(&session)).await;
        state.restore_web_model_setup_reservation();
        assert!(
            state
                .web_model
                .bindings()
                .unwrap()
                .maintenance_owner()
                .is_none()
        );
        assert_eq!(
            state.web_model_setup_status(&session).unwrap().reason,
            "creation_unconfirmed"
        );
    }
}
