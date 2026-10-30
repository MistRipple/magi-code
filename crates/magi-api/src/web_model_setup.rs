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
            "" | "start" | "continue" | "cancel"
        ) {
            return Err("配置命令不接受凭据，请使用配置卡片的安全输入框".into());
        }
        // 每个 invocation 都重新验证插件，停用 / 撤权不能继续配置。
        if !self.state.web_model_setup_available() {
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
            content: result
                .detail
                .clone()
                .unwrap_or_else(|| setup_message(&result).to_string()),
        })
    }
}

pub(crate) fn setup_message(status: &WebModelSetupStatus) -> &'static str {
    match (status.stage.as_str(), status.reason.as_str()) {
        (_, "user_takeover") => {
            "你已接管右侧浏览器，Magi 已停止操作。处理完成后点击「我已完成，继续」，Magi 会先重新读取页面再继续，不会重放之前的操作。"
        }
        ("credentials", "key_generation_required") => {
            "运行时密钥表单已准备。请核对组织和项目，在右侧点击 Create secret key，将密钥复制到下方安全输入框，关闭右侧结果页（Done），再点击「安全保存并继续」。不要把密钥发送到聊天中；保存后 Magi 自动配置通道与连接器。"
        }
        ("credentials", "runtime_key_required") => {
            "尚未保存运行时密钥。点击「继续准备密钥表单」，让 Magi 根据当前页面准备表单；已有仅含 Tunnels Read + Use 权限的密钥，也可直接在下方安全保存。不要把密钥发送到聊天中。"
        }
        ("connecting", _) => {
            "通道组件正在准备或连接。就绪后 Magi 会自动继续；连接失败时请根据配置卡片的提示处理后重试。"
        }
        ("complete", _) => {
            "GPT Web 配置已完成：通道运行正常，Magi 连接器已回读确认，工具目录已被 ChatGPT 读取。实际项目工具调用会在工作区会话中执行，并遵循 Magi 的权限和审批。"
        }
        ("cancelled", _) => "已结束本次配置，已保存的 Tunnel 和密钥保留。之后可以重新开始。",
        ("busy", _) => {
            "GPT Web 页面正在被其他会话或配置任务使用。请先处理占用，再继续配置；Magi 不会中断现有网页对话。"
        }
        ("failed", _) | (_, "agent_failed") | (_, "agent_write_pending") => {
            "当前步骤未能确认成功。请点击配置卡片的「我已完成，继续」，让 Magi 先重新读取页面再继续，不会直接重放操作；如需你处理，卡片会写明唯一的下一步。"
        }
        ("login", _) => {
            "请在右侧 GPT Web 页面完成 ChatGPT 登录，然后点击「我已完成，继续」。登录、验证码和双重验证由你操作。"
        }
        ("tunnel", _) => {
            "Magi 正在根据 OpenAI 平台当前页面准备专用 Tunnel。需要登录或选择组织、项目时会写明下一步。"
        }
        ("connector", _) => {
            "Magi 正在根据 ChatGPT 当前页面检查并配置 Magi 连接器。项目工具仍按现有权限与逐次审批执行。"
        }
        _ => "正在检查 GPT Web 配置。",
    }
}

/// 配置运行结束时清除取消句柄，避免后续的“结束配置”误取消已经无关的旧运行。
struct SetupRunGuard<'a>(&'a crate::web_model_channel::WebModelChannelRuntime);

impl Drop for SetupRunGuard<'_> {
    fn drop(&mut self) {
        self.0.finish_setup_run();
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
        let pending = if reason == "agent_observing" {
            serde_json::from_value::<WebModelSetupStatus>(
                self.settings_store.get_section(SETUP_SECTION),
            )
            .ok()
            .map(|record| pending_write_reason(&record).to_string())
            .unwrap_or_default()
        } else {
            String::new()
        };
        let record = WebModelSetupStatus {
            session_id: session.to_string(),
            stage: stage.into(),
            reason: if pending.is_empty() {
                reason.into()
            } else {
                pending
            },
            detail: None,
            updated_at: UtcMillis::now().0 as i64,
        };
        self.settings_store
            .set_section(SETUP_SECTION, json!(record))
            .map_err(crate::errors::settings_persistence_error)?;
        Ok(record)
    }

    /// 写操作开始前持久化未确认边界。恢复后 AI 必须先核对真实页面，不能盲目重放。
    pub(crate) fn mark_setup_agent_write(&self, session: &SessionId) -> Result<(), ApiError> {
        if let Some(mut record) = self.web_model_setup_status(session) {
            record.reason = "agent_write_pending".into();
            record.detail = None;
            record.updated_at = UtcMillis::now().0 as i64;
            self.settings_store
                .set_section(SETUP_SECTION, json!(record))
                .map_err(crate::errors::settings_persistence_error)?;
        }
        Ok(())
    }

    /// 恢复只恢复维护占用，不重放页面创建或账号写操作。
    pub fn restore_web_model_setup_reservation(&self) {
        let Ok(record) = serde_json::from_value::<WebModelSetupStatus>(
            self.settings_store.get_section(SETUP_SECTION),
        ) else {
            return;
        };
        if !matches!(record.stage.as_str(), "complete" | "cancelled" | "busy")
            && self.web_model_setup_available()
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
        if action == "cancel" {
            // 运行中的配置持有串行锁；先通知它停止，否则“结束配置”要排队到整轮跑完。
            self.web_model.cancel_running_setup();
        }
        let _serial = self.web_model.setup_lock.lock().await;
        if !self.web_model_setup_available() {
            return Err(ApiError::Forbidden(
                "请先到设置 → 插件安装并激活 GPT Web".into(),
            ));
        }
        if action != "cancel" {
            self.web_model.begin_setup_run(cancellation.clone());
        }
        let _run = SetupRunGuard(&self.web_model);
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
            if let Some(driver) = self.web_model.driver() {
                let _ = driver.end_setup_control().await;
            }
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
                detail: None,
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
        if matches!(action, "start" | "continue") {
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
                tracing::warn!(session_id = %session, "GPT Web 配置步骤未确认成功");
                let checkpoint = self.web_model_setup_status(session);
                // 失败后必须停在可继续的阶段，不能停在取消/完成这类终态。
                let stage = checkpoint
                    .as_ref()
                    .map(|record| record.stage.as_str())
                    .filter(|stage| !matches!(*stage, "cancelled" | "complete" | "busy"))
                    .unwrap_or("failed");
                let reason = if checkpoint
                    .as_ref()
                    .is_some_and(|record| record.reason == "agent_write_pending")
                {
                    "agent_write_pending"
                } else {
                    "agent_failed"
                };
                let message = match error {
                    ApiError::Conflict(message)
                    | ApiError::InvalidInput(message)
                    | ApiError::Forbidden(message)
                    | ApiError::CapabilityUnavailable { message, .. } => message,
                    _ => "配置步骤未能确认，请检查右侧页面".into(),
                };
                let detail = format!(
                    "{message}。已保留当前页面和配置，继续时将先重新观察，不会直接重放操作。"
                );
                self.save_setup_detail(session, stage, reason, &detail)
            }
        }
    }

    async fn advance_setup_inner(
        &self,
        session: &SessionId,
        previous: Option<&WebModelSetupStatus>,
        _action: &str,
        cancellation: &RunCancellation,
    ) -> Result<WebModelSetupStatus, ApiError> {
        use crate::web_model_setup_agent as agent;
        if cancellation.is_cancelled() || !self.web_model_setup_available() {
            return Err(ApiError::Conflict("配置已暂停".into()));
        }
        ensure_app_browser_session(self)?;
        let driver = self
            .web_model
            .driver()
            .ok_or_else(|| ApiError::Conflict("GPT Web 页面驱动尚未就绪".into()))?;
        // 上一次配置已结束（取消/完成/被占用）时，重新开始等同全新配置：否则卡片会停在
        // 终态，失败后用户没有任何可点的继续入口。
        let previous = previous.filter(|record| {
            !matches!(record.stage.as_str(), "cancelled" | "complete" | "busy")
        });
        let previous = previous.as_ref();
        // Only materialize the initial page. Resuming preserves the user's actual
        // page and form; the model owns all subsequent site navigation.
        if previous.is_none() {
            self.save_setup_stage(session, "login", "agent_observing")?;
            driver
                .open_page(
                    WEB_MODEL_HOME_TAB_ID,
                    &magi_web_model::chatgpt_web_home_url(),
                )
                .await
                .map_err(crate::web_model_ops::web_model_api_error)?;
        }
        for _ in 0..4 {
            let config = self.web_model.configured().unwrap_or_default();
            let status = self.web_model.status();
            if !config.tunnel_id.is_empty() && status.has_api_key && !status.ready {
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
                let status = self.web_model.status();
                if !status.ready {
                    return self.save_setup_stage(session, "connecting", &status.code);
                }
            }
            let current = self.web_model_setup_status(session);
            let stage = current
                .as_ref()
                .map(|record| record.stage.as_str())
                .unwrap_or("login");
            self.save_setup_stage(session, stage, "agent_observing")?;
            let run = agent::run(
                self,
                session,
                driver.as_ref(),
                current.as_ref(),
                cancellation,
            )
            .await;
            // 无论交接、完成、取消还是失败，运行结束就释放页面控制。
            let _ = driver.end_setup_control().await;
            let result = run?;
            match result.outcome.as_str() {
                "tunnel_ready" => {
                    let mut config = self.web_model.configured().unwrap_or_default();
                    let id = result
                        .tunnel_id
                        .filter(|id| valid_tunnel_id(id))
                        .ok_or_else(|| ApiError::Conflict("Tunnel ID 尚未回读确认".into()))?;
                    if !config.tunnel_id.is_empty() && config.tunnel_id != id {
                        return self.save_setup_detail(
                            session,
                            "tunnel",
                            "agent_handoff",
                            "页面 Tunnel 与已保存配置不同。请在安全配置卡片核对并提交对应凭据。",
                        );
                    }
                    config.tunnel_id = id;
                    self.settings_store
                        .set_section(crate::WEB_MODEL_TUNNEL_SECTION, json!(config))
                        .map_err(crate::errors::settings_persistence_error)?;
                    self.web_model
                        .clone()
                        .configure(self, Some(config), None)
                        .await
                        .map_err(ApiError::InvalidInput)?;
                    self.save_setup_stage(session, "credentials", "agent_observing")?;
                }
                "key_prepared" => {
                    // 作用域由页面回读的组织和项目组成，由 daemon 拼入说明，供用户在生成密钥前核对；
                    // 页面没显示的部分不写，不留空标签。
                    let scope = [("组织", &result.organization), ("项目", &result.project)]
                        .into_iter()
                        .filter_map(|(label, value)| {
                            value
                                .as_deref()
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(|value| format!("{label}：{value}"))
                        })
                        .collect::<Vec<_>>()
                        .join("；");
                    let detail = if scope.is_empty() {
                        result.detail.clone()
                    } else {
                        format!("{scope}。{}", result.detail)
                    };
                    return self.save_setup_detail(
                        session,
                        "credentials",
                        "key_generation_required",
                        &detail,
                    );
                }
                "user_takeover" => {
                    let stage = current
                        .as_ref()
                        .map(|record| record.stage.as_str())
                        .filter(|stage| ["login", "tunnel", "credentials", "connector"].contains(stage))
                        .unwrap_or("login");
                    return self.save_setup_detail(session, stage, "user_takeover", &result.detail);
                }
                "human_required" => {
                    let stage = result
                        .handoff_stage
                        .as_deref()
                        .filter(|stage| {
                            ["login", "tunnel", "credentials", "connector"].contains(stage)
                        })
                        .ok_or_else(|| ApiError::InvalidInput("AI 交接未提供有效阶段".into()))?;
                    return self.save_setup_detail(session, stage, "agent_handoff", &result.detail);
                }
                "connector_ready" => {
                    let status = self.web_model.status();
                    if !status.ready || !status.has_api_key {
                        return Err(ApiError::Conflict("通道尚未就绪，不能宣布配置完成".into()));
                    }
                    let record = self.save_setup_detail(session, "complete", "", &result.detail)?;
                    let _ = driver.end_setup_control().await;
                    self.web_model
                        .bindings()
                        .unwrap()
                        .release_maintenance(session.as_str());
                    return Ok(record);
                }
                _ => return Err(ApiError::InvalidInput("AI 配置检查点无效".into())),
            }
        }
        Err(ApiError::Conflict(
            "配置检查点未收敛，请检查当前页面后继续".into(),
        ))
    }

    fn save_setup_detail(
        &self,
        session: &SessionId,
        stage: &str,
        reason: &str,
        detail: &str,
    ) -> Result<WebModelSetupStatus, ApiError> {
        let mut record = self.save_setup_stage(session, stage, reason)?;
        record.detail = Some(
            crate::web_model_setup_agent::redact(detail)
                .chars()
                .take(1000)
                .collect(),
        );
        self.settings_store
            .set_section(SETUP_SECTION, json!(record))
            .map_err(crate::errors::settings_persistence_error)?;
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
        if !self.web_model_setup_available() {
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
        ) {
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
        "agent_write_pending" => "agent_write_pending",
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
        fail_open: AtomicBool,
        connector_writes: AtomicUsize,
        platform_actions: Mutex<Vec<String>>,
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
        fn open_temporary_chat<'a>(&'a self, _: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close_page<'a>(&'a self, _: &'a str) -> DriverFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn write_text<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> DriverFuture<'a, magi_web_model::WriteOutcome> {
            Box::pin(async {
                Err(WebModelError::new(
                    WebModelErrorCode::WebTunnelUnavailable,
                    "test driver",
                ))
            })
        }
        fn submit<'a>(&'a self, _: &'a str) -> DriverFuture<'a, magi_web_model::SubmitOutcome> {
            Box::pin(async {
                Err(WebModelError::new(
                    WebModelErrorCode::WebTunnelUnavailable,
                    "test driver",
                ))
            })
        }
        fn turn_state<'a>(&'a self, _: &'a str) -> DriverFuture<'a, magi_web_model::TurnState> {
            Box::pin(async {
                Err(WebModelError::new(
                    WebModelErrorCode::WebTunnelUnavailable,
                    "test driver",
                ))
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
            for action in ["start", "continue", "cancel"] {
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
            .save_setup_stage(&session, "tunnel", "agent_handoff")
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
            ("connector", "agent_handoff", false),
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
    async fn foreign_cancel_or_failed_takeover_never_erases_an_unconfirmed_write() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "cancelled", "agent_write_pending")
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
            "agent_write_pending"
        );
        driver.fail_open.store(true, Ordering::SeqCst);
        assert_eq!(
            state
                .advance_web_model_setup(&other, "start", &cancel)
                .await
                .unwrap()
                .reason,
            "agent_write_pending"
        );
        assert_eq!(
            state.web_model_setup_status(&other).unwrap().reason,
            "agent_write_pending"
        );
        assert_eq!(driver.connector_writes.load(Ordering::SeqCst), 0);
    }


    #[tokio::test]
    async fn restarting_after_cancel_never_leaves_the_card_in_a_terminal_stage() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state.save_setup_stage(&session, "cancelled", "").unwrap();
        driver.fail_open.store(true, Ordering::SeqCst);
        let record = state
            .advance_web_model_setup(&session, "start", &RunCancellation::default())
            .await
            .unwrap();
        assert!(
            !matches!(record.stage.as_str(), "cancelled" | "complete" | "busy"),
            "失败后卡片必须保留继续入口：{}",
            record.stage
        );
        assert_eq!(driver.pages.lock().unwrap().len(), 1, "重新开始会重新打开主页");
    }

    #[tokio::test]
    async fn restore_only_reserves_the_page_and_never_replays_pending_writes() {
        let (state, session, driver, _root) = fixture();
        crate::bundled_plugins::activate_bundled_for_test(
            &state,
            magi_web_model::WEB_MODEL_PLUGIN_ID,
        );
        state
            .save_setup_stage(&session, "tunnel", "agent_write_pending")
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
            "agent_write_pending"
        );
    }
}
