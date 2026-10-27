//! 第一方 GPT Web 配置命令。复用普通 Turn / canonical、页面驱动和通道服务，
//! 不借用正在配置的 GPT Web 模型，也不把任何凭据放进对话。
use std::sync::Arc;
use std::time::Duration;

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
const LOGIN_PROBE_TIMEOUT: Duration = Duration::from_secs(12);
const LOGIN_PROBE_INTERVAL: Duration = Duration::from_millis(500);

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
            "" | "start" | "continue" | "retry_tunnel" | "confirm_connector" | "cancel"
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
            "Tunnel 创建结果尚未确认。可点击「检查并重试创建」：Magi 会先查找已有 Tunnel，确认缺少时再尝试创建。也可在右侧手动完成创建后继续，或填写已有的 Tunnel id。"
        }
        "tunnel" if status.reason == "workspace_required" => {
            "右侧有多个或无法确认的 ChatGPT 工作空间。请选择要使用的工作空间，然后点击「我已完成，继续」，Magi 将提交表单并回读 Tunnel ID。"
        }
        "tunnel" if status.reason == "ambiguous_tunnel" => {
            "发现多个名为 Magi 的 Tunnel。请核对要使用的 Tunnel，点击「已有 Tunnel？手动填写」并保存对应完整 ID。"
        }
        "tunnel" if status.reason == "tunnel_id_required" => {
            "无法确认列表中哪个 Tunnel 属于 Magi。请打开详情核对名称，再点击「已有 Tunnel？手动填写」并保存对应完整 ID。"
        }
        "tunnel" if status.reason == "manual_required" => {
            "平台页面暂不支持自动操作。请在右侧手动创建 Tunnel，再点击「已有 Tunnel？手动填写」并保存完整 ID。"
        }
        "tunnel" if status.reason == "permission_or_page_required" => {
            "请确认当前组织及查看和创建 Tunnel 的权限，等待右侧页面加载完成后点击「我已完成，继续」。"
        }
        "tunnel" => {
            "已打开 OpenAI Platform。请完成登录或选择有权限的组织，然后点击「我已完成，继续」。Magi 将检查并创建专用 Tunnel。"
        }
        "credentials" if status.reason == "key_generation_required" => {
            "运行时密钥表单已准备：Magi Tunnel Runtime、30 天、仅 Tunnels Read + Use。请在右侧点击 Create secret key，将密钥复制到下方安全输入框，关闭右侧结果页（Done），再点击「安全保存并继续」。不要把密钥发送到聊天中；保存后 Magi 自动配置通道与连接器。"
        }
        "credentials" if status.reason == "project_required" => {
            "密钥表单中有多个或无法确认的项目。请在右侧选择要使用的项目，再点击「继续准备密钥表单」；其余权限由 Magi 配置。"
        }
        "credentials" if status.reason == "key_permissions_required" => {
            "密钥权限尚未确认。请保留 Restricted 且仅启用 Tunnels 的 Read 与 Use，再点击「继续准备密钥表单」检查。"
        }
        "credentials" if status.reason == "login_required" => {
            "请在右侧完成 OpenAI 平台登录或验证，再点击「继续准备密钥表单」。无需退出 ChatGPT 登录。"
        }
        "credentials" => {
            "密钥表单尚未准备完成。点击「继续准备密钥表单」，让 Magi 检查项目和权限；已有仅含 Tunnels Read + Use 权限的密钥，也可直接在下方安全保存。不要把密钥发送到聊天中。"
        }
        "connecting" => {
            "通道组件正在准备或连接。就绪后 Magi 会自动继续；连接失败时请根据配置卡片的提示处理后重试。"
        }
        "connector" if status.reason == "connector_access_denied" => {
            "ChatGPT 拒绝了当前 Tunnel 的访问。请在 OpenAI 平台确认 Tunnel 对当前 ChatGPT 工作空间开放；切换账号后尤其需要检查。处理后再检查并重试连接器。"
        }
        "connector" if status.reason == "connector_ambiguous" => {
            "发现多个名为 Magi 的连接器，无法安全选择。请在右侧核对连接器，再检查并重试；Magi 不会自动新建或刷新不明确的应用。"
        }
        "connector" if status.reason == "connector_tunnel_mismatch" => {
            "连接器详情中的 Tunnel ID 与本次配置不一致。请在右侧核对连接器和 Tunnel，再检查并重试。"
        }
        "connector" if status.reason == "connector_refresh_rejected" => {
            "ChatGPT 刷新工具时报告错误，尚未确认刷新成功。请检查右侧页面与工具通道，再检查并重试连接器。"
        }
        "connector"
            if matches!(
                status.reason.as_str(),
                "connector_page_not_open"
                    | "connector_not_listed"
                    | "connector_detail_unconfirmed"
                    | "connector_refresh_button_missing"
            ) =>
        {
            "连接器页面在等待时限内仍未就绪，尚未确认本次配置。请检查右侧页面，再检查并重试连接器。"
        }
        "connector"
            if connector_failure_reason(Some(&status.reason)).is_some()
                || matches!(
                    status.reason.as_str(),
                    "connector_write_started"
                        | "connector_unconfirmed"
                        | "connector_refresh_unconfirmed"
                        | "connector_access_denied"
                ) =>
        {
            "连接器配置结果尚未确认。请在右侧完成后继续，或点击「检查并重试连接器」重新授权。Magi 会先检查，缺少时创建，已有连接器需要刷新时重试刷新。"
        }
        "connector" if status.reason == "confirmation_required" => {
            "点击「允许配置连接器并继续」，Magi 将检查并配置 ChatGPT 的 Magi 连接器。项目工具仍按现有权限与逐次审批执行。"
        }
        "connector" => {
            "正在检查并配置 ChatGPT 的 Magi 连接器。项目工具仍按现有权限与逐次审批执行。"
        }
        "complete" => {
            "GPT Web 配置已完成：通道运行正常，Magi 连接器已回读确认。实际项目工具调用会在工作区会话中执行，并遵循 Magi 的权限和审批。"
        }
        "cancelled" => "已结束本次配置，已保存的 Tunnel 和密钥保留。之后可以重新开始。",
        "busy" => {
            "GPT Web 页面正在被其他会话或配置任务使用。请先处理占用，再继续配置；Magi 不会中断现有网页对话。"
        }
        "failed" => {
            "当前步骤未能确认成功。请点击配置卡片的「我已完成，继续」，让 Magi 重新检查已保存配置和页面状态；如需你处理，卡片会显示具体步骤。"
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
            let previous = serde_json::from_value::<WebModelSetupStatus>(
                self.settings_store.get_section(SETUP_SECTION),
            )
            .ok();
            if previous.as_ref().is_some_and(|record| {
                record.session_id != session.as_str() && !pending_write_reason(record).is_empty()
            }) {
                return Err(ApiError::Conflict(
                    "请回到原配置会话结束配置，不能覆盖另一会话未确认的操作".into(),
                ));
            }
            let reason = previous
                .as_ref()
                .map(pending_write_reason)
                .unwrap_or_default()
                .to_string();
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
        // 转交配置拥有者时先保存未确认边界；随后导航失败也不能丢失旧会话的检查点。
        if let Some(record) = existing
            .as_ref()
            .filter(|record| record.session_id != session.as_str())
        {
            self.save_setup_stage(session, &record.stage, &record.reason)?;
        }
        if matches!(
            action,
            "start" | "continue" | "retry_tunnel" | "confirm_connector"
        ) {
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
                if let Some(checkpoint) = checkpoint.filter(|record| {
                    !pending_write_reason(record).is_empty() || record.stage == "credentials"
                }) {
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
        let map_error = |error: magi_web_model::WebModelError| {
            tracing::warn!(session_id = %session, code = %error.code.code(), "GPT Web 连接器宿主命令失败");
            crate::web_model_ops::web_model_api_error(error)
        };
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
            // 导航返回时页面可能还在渲染；有界轮询站点会话探测，不把空白加载态当成退出登录。
            // 输入框不能证明登录：ChatGPT 的访客页面也可能提供输入框。
            let deadline = tokio::time::Instant::now() + LOGIN_PROBE_TIMEOUT;
            let page = loop {
                check()?;
                let page = driver
                    .probe_page(WEB_MODEL_HOME_TAB_ID)
                    .await
                    .map_err(map_error)?;
                if page.login_state == magi_web_model::LoginState::Blocked
                    || (page.login_state == magi_web_model::LoginState::SignedIn
                        && page.composer_available)
                    || tokio::time::Instant::now() >= deadline
                {
                    break page;
                }
                tokio::time::sleep(LOGIN_PROBE_INTERVAL).await;
            };
            if page.login_state != magi_web_model::LoginState::SignedIn || !page.composer_available
            {
                let reason = previous
                    .map(pending_write_reason)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or(if page.login_state == magi_web_model::LoginState::Blocked {
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
            // 未确认的写入默认只回读；用户明确重试时也先检查，不能直接重放创建。
            let inspect_only = previous.is_some_and(|record| {
                matches!(
                    record.reason.as_str(),
                    "creation_started" | "creation_unconfirmed"
                )
            });
            self.save_setup_stage(session, "tunnel", "creation_started")?;
            let mut value = driver
                .platform_setup(if inspect_only {
                    "inspect_tunnel"
                } else {
                    "create_tunnel"
                })
                .await
                .map_err(map_error)?;
            if inspect_only
                && action == "retry_tunnel"
                && value.get("status").and_then(serde_json::Value::as_str) == Some("missing")
            {
                check()?;
                self.save_setup_stage(session, "tunnel", "creation_started")?;
                value = driver
                    .platform_setup("create_tunnel")
                    .await
                    .map_err(map_error)?;
            }
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
                    if inspect_only && reason != "workspace_required" {
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
            self.save_setup_stage(session, "credentials", "runtime_key_required")?;
            driver
                .open_page(
                    WEB_MODEL_HOME_TAB_ID,
                    &magi_web_model::openai_platform_api_keys_url(),
                )
                .await
                .map_err(map_error)?;
            check()?;
            let prepared = driver
                .platform_setup("prepare_runtime_key")
                .await
                .map_err(map_error)?;
            let reason = match prepared.get("status").and_then(serde_json::Value::as_str) {
                Some("key_generation_required") => "key_generation_required",
                Some("project_required") => "project_required",
                Some("key_permissions_required") => "key_permissions_required",
                Some("login_required") => "login_required",
                _ => "runtime_key_required",
            };
            return self.save_setup_stage(session, "credentials", reason);
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
        let map_error = |error: magi_web_model::WebModelError| {
            tracing::warn!(session_id = %session, code = %error.code.code(), "GPT Web 连接器宿主命令失败");
            crate::web_model_ops::web_model_api_error(error)
        };
        check()?;
        let mut inspected = None;
        // 默认只回读；显式再次授权且确认不存在时，才允许重试创建。
        if previous.is_some_and(|record| {
            connector_failure_reason(Some(&record.reason)).is_some()
                || matches!(
                    record.reason.as_str(),
                    "connector_write_started"
                        | "connector_unconfirmed"
                        | "connector_refresh_unconfirmed"
                        | "connector_access_denied"
                )
        }) {
            let outcome = driver
                .connector_status(WEB_MODEL_HOME_TAB_ID, "Magi")
                .await
                .map_err(map_error)?;
            check()?;
            let refresh_pending = previous.is_some_and(|record| {
                record.reason == "connector_refresh_unconfirmed"
                    || (connector_failure_reason(Some(&record.reason)).is_some()
                        && record.reason != "connector_unconfirmed")
            });
            let access_denied =
                previous.is_some_and(|record| record.reason == "connector_access_denied");
            // 安全卡片可能更换了 Tunnel/密钥；旧连接器已启用不能证明新配置可访问。
            if outcome.supported
                && outcome.exists
                && outcome.enabled
                && !refresh_pending
                && !access_denied
            {
                let record = self.save_setup_stage(session, "complete", "")?;
                self.web_model
                    .bindings()
                    .unwrap()
                    .release_maintenance(session.as_str());
                return Ok(record);
            }
            let can_retry = action == "confirm_connector"
                && outcome.supported
                && (!outcome.exists || !outcome.enabled || refresh_pending || access_denied)
                && previous.is_some_and(|record| record.stage == "connector");
            if !can_retry {
                let reason = if refresh_pending {
                    previous
                        .map(|record| record.reason.as_str())
                        .unwrap_or("connector_refresh_unconfirmed")
                } else if previous.is_some_and(|record| record.reason == "connector_access_denied")
                {
                    "connector_access_denied"
                } else {
                    "connector_unconfirmed"
                };
                return self.save_setup_stage(session, "connector", reason);
            }
            inspected = Some(outcome);
        }
        // 发起配置即授权创建/刷新专用连接器；仅未确认写入仍要求显式重试。
        check()?;
        // 刷新已有连接器也可能断连；写入前区分刷新，避免恢复时把旧启用状态当成本次刷新成功。
        let before_write = match inspected {
            Some(outcome) => outcome,
            None => driver
                .connector_status(WEB_MODEL_HOME_TAB_ID, "Magi")
                .await
                .map_err(map_error)?,
        };
        check()?;
        if !before_write.supported {
            let reason = connector_failure_reason(before_write.reason.as_deref())
                .unwrap_or("connector_unconfirmed");
            return self.save_setup_stage(session, "connector", reason);
        }
        let checkpoint = if before_write.exists {
            "connector_refresh_unconfirmed"
        } else {
            "connector_write_started"
        };
        self.save_setup_stage(session, "connector", checkpoint)?;
        let outcome = driver
            .configure_connector(WEB_MODEL_HOME_TAB_ID, "Magi", tunnel_id)
            .await
            .map_err(map_error)?;
        check()?;
        if !outcome.configured || !outcome.confirmed_enabled {
            let reason = connector_failure_reason(outcome.reason.as_deref()).unwrap_or(
                if outcome.configured {
                    "connector_refresh_unconfirmed"
                } else {
                    "connector_unconfirmed"
                },
            );
            return self.save_setup_stage(session, "connector", reason);
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
        if !matches!(
            record.stage.as_str(),
            "credentials" | "tunnel" | "connecting"
        ) && !(record.stage == "connector" && record.reason == "connector_access_denied")
        {
            return Err(ApiError::Conflict("当前步骤不接受凭据".into()));
        }
        let mut config = self.web_model.configured().unwrap_or_default();
        let previous_tunnel_id = config.tunnel_id.clone();
        if let Some(id) = tunnel_id.filter(|id| !id.trim().is_empty()) {
            if !valid_tunnel_id(id.trim()) {
                return Err(ApiError::InvalidInput("Tunnel id 格式不正确".into()));
            }
            config.tunnel_id = id.trim().into();
        }
        if config.tunnel_id.is_empty() {
            return Err(ApiError::InvalidInput("请先填写 Tunnel id".into()));
        }
        if config.tunnel_id != previous_tunnel_id
            && self.web_model.status().has_api_key
            && api_key.trim().is_empty()
        {
            return Err(ApiError::InvalidInput(
                "更换 Tunnel ID 时请一并填写对应的运行时密钥".into(),
            ));
        }
        if !api_key.trim().is_empty() {
            crate::web_model_channel::validate_api_key(&api_key).map_err(ApiError::InvalidInput)?;
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
        if self.web_model.status().has_api_key {
            self.save_setup_stage(session, "connecting", pending_write_reason(&record))?;
        } else {
            self.save_setup_stage(session, "credentials", "runtime_key_required")?;
        }
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
        "connector_access_denied" => "connector_access_denied",
        "connector_refresh_unconfirmed" => "connector_refresh_unconfirmed",
        reason if connector_failure_reason(Some(reason)).is_some() => reason,
        _ => "",
    }
}

/// 页面只允许这些固定原因进入配置记录；任意异常正文或凭据绝不持久化。
pub(crate) fn connector_failure_reason(reason: Option<&str>) -> Option<&'static str> {
    const REASONS: &[&str] = &[
        "connector_unconfirmed",
        "connector_access_denied",
        "connector_page_not_open",
        "connector_ambiguous",
        "connector_not_listed",
        "connector_detail_unconfirmed",
        "connector_refresh_button_missing",
        "connector_refresh_unconfirmed",
        "connector_refresh_rejected",
        "connector_tunnel_mismatch",
        "connector_create_entry_missing",
        "connector_form_fields_missing",
        "connector_tunnel_option_missing",
        "connector_auth_option_missing",
        "connector_save_button_missing",
        "connector_create_rejected",
        "connector_not_enabled_after_create",
        "connector_not_listed_after_create",
    ];
    REASONS
        .iter()
        .copied()
        .find(|candidate| Some(*candidate) == reason)
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
        fail_open: AtomicBool,
        connector_enabled: AtomicBool,
        connector_exists: AtomicBool,
        connector_unsupported: AtomicBool,
        connector_refresh_failed: AtomicBool,
        connector_access_denied: AtomicBool,
        connector_failure: Mutex<Option<String>>,
        connector_writes: AtomicUsize,
        key_preparation_failed: AtomicBool,
        platform_actions: Mutex<Vec<String>>,
        platform_results: Mutex<Vec<serde_json::Value>>,
        pages: Mutex<Vec<String>>,
        probes: Mutex<Vec<magi_web_model::PageProbe>>,
    }

    impl WebModelPageDriver for SetupDriver {
        fn probe_page<'a>(&'a self, _: &'a str) -> DriverFuture<'a, magi_web_model::PageProbe> {
            Box::pin(async move {
                let mut probes = self.probes.lock().unwrap();
                if !probes.is_empty() {
                    return Ok(probes.remove(0));
                }
                Ok(magi_web_model::PageProbe {
                    login_state: if self.logged_in.load(Ordering::SeqCst) {
                        magi_web_model::LoginState::SignedIn
                    } else {
                        magi_web_model::LoginState::SignedOut
                    },
                    composer_available: true,
                })
            })
        }
        fn open_page<'a>(&'a self, _: &'a str, url: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async move {
                self.pages.lock().unwrap().push(url.into());
                if self.fail_open.load(Ordering::SeqCst) {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebTunnelUnavailable,
                        "SECRET_DIAGNOSTIC",
                    ));
                }
                Ok(())
            })
        }
        fn resume_page<'a>(&'a self, _: &'a str) -> DriverFuture<'a, bool> {
            Box::pin(async { Ok(self.logged_in.load(Ordering::SeqCst)) })
        }
        fn platform_setup<'a>(&'a self, action: &'a str) -> DriverFuture<'a, serde_json::Value> {
            Box::pin(async move {
                self.platform_actions.lock().unwrap().push(action.into());
                if action == "prepare_runtime_key" {
                    if self.key_preparation_failed.load(Ordering::SeqCst) {
                        return Err(WebModelError::new(
                            WebModelErrorCode::WebTunnelUnavailable,
                            "SECRET_DIAGNOSTIC",
                        ));
                    }
                    return Ok(json!({"status":"key_generation_required"}));
                }
                {
                    let mut results = self.platform_results.lock().unwrap();
                    if !results.is_empty() {
                        return Ok(results.remove(0));
                    }
                }
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
                    supported: !self.connector_unsupported.load(Ordering::SeqCst),
                    exists: self.connector_exists.load(Ordering::SeqCst)
                        || self.connector_enabled.load(Ordering::SeqCst),
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
                if let Some(reason) = self.connector_failure.lock().unwrap().clone() {
                    return Ok(magi_web_model::ConnectorConfigOutcome {
                        configured: true,
                        confirmed_enabled: false,
                        reason: Some(reason),
                    });
                }
                if self.connector_access_denied.load(Ordering::SeqCst) {
                    return Ok(magi_web_model::ConnectorConfigOutcome {
                        configured: false,
                        confirmed_enabled: false,
                        reason: Some("connector_access_denied".into()),
                    });
                }
                if self.connector_refresh_failed.load(Ordering::SeqCst) {
                    return Ok(magi_web_model::ConnectorConfigOutcome {
                        configured: true,
                        confirmed_enabled: false,
                        reason: Some("connector_refresh_unconfirmed".into()),
                    });
                }
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
    async fn setup_plugin_gate_blocks_every_action_and_credentials_without_page_writes() {
        let (state, session, driver, _root) = fixture();
        let package =
            crate::bundled_plugins::bundled_plugin_package(magi_web_model::WEB_MODEL_PLUGIN_ID)
                .unwrap();
        let id = magi_web_model::WEB_MODEL_PLUGIN_ID;
        let cancel = RunCancellation::default();
        for phase in [
            "absent",
            "installed",
            "authorized",
            "enabled",
            "deactivated",
            "uninstalled",
        ] {
            {
                let mut manager = state.plugin_manager.lock().unwrap();
                match phase {
                    "installed" => {
                        manager
                            .install(
                                &package,
                                magi_plugin_system::PluginSource::Builtin { name: id.into() },
                            )
                            .unwrap();
                    }
                    "authorized" => manager
                        .authorize(id, "application", package.manifest().permissions.clone())
                        .unwrap(),
                    "enabled" => manager.enable(id, "application").unwrap(),
                    "deactivated" => {
                        manager.activate(id, "application").unwrap();
                        manager.deactivate(id, "application").unwrap();
                    }
                    "uninstalled" => {
                        manager.disable(id, "application").unwrap();
                        manager.uninstall(id).unwrap();
                    }
                    _ => {}
                }
            }
            for action in [
                "start",
                "continue",
                "retry_tunnel",
                "confirm_connector",
                "cancel",
            ] {
                assert!(
                    matches!(
                        state
                            .advance_web_model_setup(&session, action, &cancel)
                            .await,
                        Err(ApiError::Forbidden(_))
                    ),
                    "{phase}: {action}"
                );
            }
            assert!(
                matches!(
                    state
                        .save_setup_credentials(
                            &session,
                            Some("tunnel_0123456789abcdef0123456789abcdef".into()),
                            "sk-fixture-secret".into()
                        )
                        .await,
                    Err(ApiError::Forbidden(_))
                ),
                "{phase}: credentials"
            );
            assert!(driver.pages.lock().unwrap().is_empty(), "{phase}");
            assert!(
                driver.platform_actions.lock().unwrap().is_empty(),
                "{phase}"
            );
            assert!(
                state.settings_store.get_section(SETUP_SECTION).is_null(),
                "{phase}"
            );
            assert!(state.web_model.configured().is_none(), "{phase}");
        }
    }

    #[tokio::test]
    async fn setup_waits_for_login_probe_after_navigation() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        driver.probes.lock().unwrap().extend([
            magi_web_model::PageProbe {
                login_state: magi_web_model::LoginState::SignedOut,
                composer_available: false,
            },
            // 访客也可能有输入框，必须继续等站点会话确认。
            magi_web_model::PageProbe {
                login_state: magi_web_model::LoginState::SignedOut,
                composer_available: true,
            },
            magi_web_model::PageProbe {
                login_state: magi_web_model::LoginState::SignedIn,
                composer_available: true,
            },
        ]);
        let result = state
            .advance_web_model_setup(&session, "start", &RunCancellation::default())
            .await
            .unwrap();
        assert_eq!(result.stage, "credentials");
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            ["create_tunnel", "prepare_runtime_key"]
        );
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
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            ["create_tunnel", "prepare_runtime_key"]
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
    async fn key_form_takeover_keeps_credentials_checkpoint_for_safe_resume() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        driver.logged_in.store(true, Ordering::SeqCst);
        driver.key_preparation_failed.store(true, Ordering::SeqCst);
        let result = state
            .advance_web_model_setup(&session, "start", &RunCancellation::default())
            .await
            .unwrap();
        assert_eq!(result.stage, "credentials");
        assert_eq!(result.reason, "runtime_key_required");
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            ["create_tunnel", "prepare_runtime_key"]
        );
        driver.key_preparation_failed.store(false, Ordering::SeqCst);
        let result = state
            .advance_web_model_setup(&session, "continue", &RunCancellation::default())
            .await
            .unwrap();
        assert_eq!(result.reason, "key_generation_required");
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            [
                "create_tunnel",
                "prepare_runtime_key",
                "prepare_runtime_key"
            ]
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
    async fn explicit_tunnel_retry_checks_before_creating_and_recovers_old_checkpoint() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "tunnel", "creation_unconfirmed")
            .unwrap();
        driver
            .platform_results
            .lock()
            .unwrap()
            .push(json!({"status":"missing"}));
        let status = state
            .advance_web_model_setup(&session, "retry_tunnel", &RunCancellation::default())
            .await
            .unwrap();
        assert_eq!(status.stage, "credentials");
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            ["inspect_tunnel", "create_tunnel", "prepare_runtime_key"]
        );
        assert!(!state.web_model.configured().unwrap().tunnel_id.is_empty());
    }

    #[tokio::test]
    async fn explicit_tunnel_retry_preserves_workspace_handoff_instructions() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "tunnel", "creation_unconfirmed")
            .unwrap();
        driver.platform_results.lock().unwrap().extend([
            json!({"status":"missing"}),
            json!({"status":"workspace_required"}),
        ]);
        let status = state
            .advance_web_model_setup(&session, "retry_tunnel", &RunCancellation::default())
            .await
            .unwrap();
        assert_eq!(status.stage, "tunnel");
        assert_eq!(status.reason, "workspace_required");
        assert!(setup_message(&status).contains("工作空间"));
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            ["inspect_tunnel", "create_tunnel"]
        );
        assert!(
            state
                .web_model
                .configured()
                .unwrap_or_default()
                .tunnel_id
                .is_empty()
        );
    }

    #[tokio::test]
    async fn explicit_tunnel_retry_never_creates_when_inspection_is_inconclusive_or_ready() {
        for reason in [
            "login_required",
            "page_required",
            "manual_required",
            "ambiguous_tunnel",
            "permission_or_page_required",
            "tunnel_id_required",
            "ready",
        ] {
            let (state, session, driver, _root) = fixture();
            crate::bundled_plugins::activate_bundled_for_test(
                &state,
                magi_web_model::WEB_MODEL_PLUGIN_ID,
            );
            state
                .save_setup_stage(&session, "tunnel", "creation_started")
                .unwrap();
            let result = if reason == "ready" {
                json!({"status":"ready", "tunnel_id":"tunnel_0123456789abcdef0123456789abcdef"})
            } else {
                json!({"status":reason})
            };
            driver.platform_results.lock().unwrap().push(result);
            let status = state
                .advance_web_model_setup(&session, "retry_tunnel", &RunCancellation::default())
                .await
                .unwrap();
            assert_eq!(
                *driver.platform_actions.lock().unwrap(),
                if reason == "ready" {
                    vec!["inspect_tunnel", "prepare_runtime_key"]
                } else {
                    vec!["inspect_tunnel"]
                },
                "{reason}"
            );
            assert_eq!(
                status.stage,
                if reason == "ready" {
                    "credentials"
                } else {
                    "tunnel"
                }
            );
            if reason != "ready" {
                assert_eq!(status.reason, "creation_unconfirmed");
            }
        }
    }

    #[tokio::test]
    async fn failed_explicit_tunnel_retry_preserves_no_automatic_replay_boundary() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "tunnel", "creation_started")
            .unwrap();
        driver.lose_creation_result.store(true, Ordering::SeqCst);
        let cancel = RunCancellation::default();
        assert_eq!(
            state
                .advance_web_model_setup(&session, "retry_tunnel", &cancel)
                .await
                .unwrap()
                .reason,
            "creation_started"
        );
        state
            .advance_web_model_setup(&session, "continue", &cancel)
            .await
            .unwrap();
        assert_eq!(
            *driver.platform_actions.lock().unwrap(),
            ["inspect_tunnel", "create_tunnel", "inspect_tunnel"]
        );
    }

    #[tokio::test]
    async fn connector_automatically_configures_and_recovers_unconfirmed_write_by_reading() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        let slots = state.web_model.bindings().unwrap();
        slots.reserve_maintenance(session.as_str()).unwrap();
        let cancel = RunCancellation::default();
        // No extra confirmation round is needed after the user starts setup.
        assert!(
            state
                .advance_setup_connector(
                    &session,
                    None,
                    "continue",
                    "tunnel_0123456789abcdef0123456789abcdef",
                    driver.as_ref(),
                    &cancel,
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
                "continue",
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
    async fn installed_but_unlinked_connector_is_not_complete_and_explicit_retry_resumes_configuring()
     {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .web_model
            .bindings()
            .unwrap()
            .reserve_maintenance(session.as_str())
            .unwrap();
        driver.connector_exists.store(true, Ordering::SeqCst);
        let previous = state
            .save_setup_stage(&session, "connector", "connector_unconfirmed")
            .unwrap();
        let cancel = RunCancellation::default();
        let waiting = state
            .advance_setup_connector(
                &session,
                Some(&previous),
                "continue",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(waiting.stage, "connector");
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 0);
        assert!(
            state
                .advance_setup_connector(
                    &session,
                    Some(&waiting),
                    "confirm_connector",
                    "tunnel_0123456789abcdef0123456789abcdef",
                    driver.as_ref(),
                    &cancel
                )
                .await
                .is_err()
        );
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.web_model_setup_status(&session).unwrap().stage,
            "connector"
        );
    }

    #[tokio::test]
    async fn connector_access_denial_stays_actionable_without_replaying_on_continue() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        driver.connector_access_denied.store(true, Ordering::SeqCst);
        let slots = state.web_model.bindings().unwrap();
        slots.reserve_maintenance(session.as_str()).unwrap();
        let before = state
            .save_setup_stage(&session, "connector", "confirmation_required")
            .unwrap();
        let denied = state
            .advance_setup_connector(
                &session,
                Some(&before),
                "confirm_connector",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &RunCancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(denied.reason, "connector_access_denied");
        assert!(setup_message(&denied).contains("工作空间"));
        assert_eq!(pending_write_reason(&denied), "connector_access_denied");
        let waiting = state
            .advance_setup_connector(
                &session,
                Some(&denied),
                "continue",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &RunCancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(waiting.reason, "connector_access_denied");
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 1);
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
    async fn manual_tunnel_without_key_waits_for_credentials_and_invalid_key_does_not_mutate_config()
     {
        let (state, session, _driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .web_model
            .bindings()
            .unwrap()
            .reserve_maintenance(session.as_str())
            .unwrap();
        state
            .save_setup_stage(&session, "tunnel", "manual_required")
            .unwrap();
        let id = "tunnel_0123456789abcdef0123456789abcdef";
        state
            .save_setup_credentials(&session, Some(id.into()), String::new())
            .await
            .unwrap();
        assert_eq!(
            state.web_model_setup_status(&session).unwrap().stage,
            "credentials"
        );
        assert!(!state.web_model.status().has_api_key);
        assert!(
            state
                .save_setup_credentials(
                    &session,
                    Some("tunnel_abcdef0123456789abcdef0123456789".into()),
                    "sk-invalid key".into()
                )
                .await
                .is_err()
        );
        assert_eq!(state.web_model.configured().unwrap().tunnel_id, id);
        assert_eq!(
            state
                .settings_store
                .get_section(crate::WEB_MODEL_TUNNEL_SECTION)["tunnelId"],
            id
        );
        assert_eq!(
            state.web_model_setup_status(&session).unwrap().stage,
            "credentials"
        );
        // 已保存密钥不能在更换 Tunnel 时被静默复用，避免把旧账号凭据绑到新通道。
        let key_path = crate::web_model_channel::api_key_path(_root.path());
        std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        std::fs::write(&key_path, "sk-existing-test-key").unwrap();
        assert!(state.web_model.status().has_api_key);
        state
            .save_setup_stage(&session, "connecting", "client_exit")
            .unwrap();
        assert!(
            state
                .save_setup_credentials(
                    &session,
                    Some("tunnel_abcdef0123456789abcdef0123456789".into()),
                    String::new(),
                )
                .await
                .is_err()
        );
        assert_eq!(state.web_model.configured().unwrap().tunnel_id, id);
    }

    #[tokio::test]
    async fn inline_credentials_can_repair_connection_stages_but_not_reopen_completed_setup() {
        for (stage, reason, allowed) in [
            ("connecting", "client_exit", true),
            ("connector", "connector_access_denied", true),
            ("connector", "connector_write_started", false),
            ("complete", "", false),
            ("cancelled", "", false),
        ] {
            let (state, session, _driver, _root) = fixture();
            crate::bundled_plugins::activate_bundled_for_test(
                &state,
                magi_web_model::WEB_MODEL_PLUGIN_ID,
            );
            state
                .web_model
                .bindings()
                .unwrap()
                .reserve_maintenance(session.as_str())
                .unwrap();
            state.save_setup_stage(&session, stage, reason).unwrap();
            let result = state
                .save_setup_credentials(
                    &session,
                    Some("tunnel_0123456789abcdef0123456789abcdef".into()),
                    String::new(),
                )
                .await;
            assert_eq!(result.is_ok(), allowed, "{stage}/{reason}");
            if allowed {
                assert_eq!(
                    state.web_model_setup_status(&session).unwrap().stage,
                    "credentials"
                );
            } else {
                assert!(
                    state
                        .settings_store
                        .get_section(crate::WEB_MODEL_TUNNEL_SECTION)
                        .is_null()
                );
            }
        }
    }

    #[tokio::test]
    async fn access_denied_never_completes_from_old_enabled_connector_after_credentials_change() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .web_model
            .bindings()
            .unwrap()
            .reserve_maintenance(session.as_str())
            .unwrap();
        driver.connector_enabled.store(true, Ordering::SeqCst);
        let before = state
            .save_setup_stage(&session, "connecting", "connector_access_denied")
            .unwrap();
        let waiting = state
            .advance_setup_connector(
                &session,
                Some(&before),
                "continue",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &RunCancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(waiting.stage, "connector");
        assert_eq!(waiting.reason, "connector_access_denied");
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 0);
        assert!(
            state
                .web_model
                .bindings()
                .unwrap()
                .maintenance_owner()
                .is_some()
        );
        let _ = state
            .advance_setup_connector(
                &session,
                Some(&waiting),
                "confirm_connector",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &RunCancellation::default(),
            )
            .await;
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn foreign_cancel_or_failed_takeover_never_erases_an_unconfirmed_write() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "cancelled", "connector_unconfirmed")
            .unwrap();
        let other = SessionId::new("another-setup");
        state
            .session_store
            .create_session(other.clone(), "Other")
            .unwrap();
        let cancel = RunCancellation::default();
        assert!(
            state
                .advance_web_model_setup(&other, "cancel", &cancel)
                .await
                .is_err()
        );
        assert_eq!(
            state.web_model_setup_status(&session).unwrap().reason,
            "connector_unconfirmed"
        );
        driver.fail_open.store(true, Ordering::SeqCst);
        assert_eq!(
            state
                .advance_web_model_setup(&other, "start", &cancel)
                .await
                .unwrap()
                .reason,
            "connector_unconfirmed"
        );
        assert_eq!(
            state.web_model_setup_status(&other).unwrap().reason,
            "connector_unconfirmed"
        );
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn connector_fixed_failure_reasons_survive_without_replaying_or_leaking_page_text() {
        for reason in [
            "connector_refresh_rejected",
            "connector_tunnel_mismatch",
            "connector_detail_unconfirmed",
            "SECRET_PAGE_TEXT",
        ] {
            let (state, session, driver, _root) = fixture();
            crate::bundled_plugins::activate_bundled_for_test(
                &state,
                magi_web_model::WEB_MODEL_PLUGIN_ID,
            );
            driver.connector_enabled.store(true, Ordering::SeqCst);
            *driver.connector_failure.lock().unwrap() = Some(reason.into());
            let cancel = RunCancellation::default();
            let failed = state
                .advance_setup_connector(
                    &session,
                    None,
                    "continue",
                    "tunnel_0123456789abcdef0123456789abcdef",
                    driver.as_ref(),
                    &cancel,
                )
                .await
                .unwrap();
            let expected = if reason == "SECRET_PAGE_TEXT" {
                "connector_refresh_unconfirmed"
            } else {
                reason
            };
            assert_eq!(failed.reason, expected);
            assert_eq!(pending_write_reason(&failed), expected);
            let waiting = state
                .advance_setup_connector(
                    &session,
                    Some(&failed),
                    "continue",
                    "tunnel_0123456789abcdef0123456789abcdef",
                    driver.as_ref(),
                    &cancel,
                )
                .await
                .unwrap();
            assert_eq!(waiting.reason, expected);
            assert_eq!(waiting.stage, "connector");
            assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 1);
            assert!(!setup_message(&waiting).contains("SECRET_PAGE_TEXT"));
        }
    }

    #[tokio::test]
    async fn connector_retry_requires_explicit_consent_and_supported_page() {
        for (unsupported, exists, action, expected_writes) in [
            (false, false, "continue", 0),
            (true, false, "confirm_connector", 0),
            // 已安装但未关联时接续连接，页面驱动不会再次创建应用。
            (false, true, "confirm_connector", 1),
            (false, false, "confirm_connector", 1),
        ] {
            let (state, session, driver, _root) = fixture();
            crate::bundled_plugins::activate_bundled_for_test(
                &state,
                magi_web_model::WEB_MODEL_PLUGIN_ID,
            );
            driver
                .connector_unsupported
                .store(unsupported, Ordering::SeqCst);
            driver.connector_exists.store(exists, Ordering::SeqCst);
            let previous = state
                .save_setup_stage(&session, "connector", "connector_unconfirmed")
                .unwrap();
            let _ = state
                .advance_setup_connector(
                    &session,
                    Some(&previous),
                    action,
                    "tunnel_0123456789abcdef0123456789abcdef",
                    driver.as_ref(),
                    &RunCancellation::default(),
                )
                .await;
            assert_eq!(
                driver.connector_writes.load(Ordering::SeqCst),
                expected_writes
            );
        }
    }

    #[tokio::test]
    async fn enabled_connector_does_not_hide_failed_refresh_and_retry_is_explicit() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        driver.connector_enabled.store(true, Ordering::SeqCst);
        driver
            .connector_refresh_failed
            .store(true, Ordering::SeqCst);
        let previous = state
            .save_setup_stage(&session, "connector", "confirmation_required")
            .unwrap();
        let cancel = RunCancellation::default();
        let failed = state
            .advance_setup_connector(
                &session,
                Some(&previous),
                "confirm_connector",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(failed.reason, "connector_refresh_unconfirmed");
        let still_failed = state
            .advance_setup_connector(
                &session,
                Some(&failed),
                "continue",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(still_failed.reason, "connector_refresh_unconfirmed");
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 1);
        state
            .advance_setup_connector(
                &session,
                Some(&still_failed),
                "confirm_connector",
                "tunnel_0123456789abcdef0123456789abcdef",
                driver.as_ref(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 2);
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
