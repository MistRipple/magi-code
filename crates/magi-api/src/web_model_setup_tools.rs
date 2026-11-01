//! 主对话配置工具。只提交领域事实，不选择模型或运行隐藏循环。
use crate::web_model_ops::{WEB_MODEL_HOME_TAB_ID, ensure_app_browser_session};
use crate::{ApiError, ApiState, web_model_setup_policy as policy};
use magi_app_server_protocol::generated::WebModelSetupStatus;
use magi_core::{ExecutionResultStatus, SessionId, TaskId};
use magi_plugin_system::{APPLICATION_SCOPE, PluginRuntimeLease, RunCancellation};
use magi_tool_runtime::{ToolCallerScope, ToolExecutionContext};
use serde_json::{Value, json};

pub const SKILL_ID: &str = "plugin/openai.chatgpt-web/gpt-web-setup";
pub const TOOL_ID: &str = "configure";
pub fn tool_name() -> String {
    magi_plugin_system::model_tool_name(magi_web_model::WEB_MODEL_PLUGIN_ID, TOOL_ID)
}
pub fn tool_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["action"],"properties":{
        "action":{"type":"string","enum":["inspect","begin","checkpoint","pause","cancel","transfer"]},
        "expected_revision":{"type":"integer","minimum":0},"setup_run_id":{"type":"string"},
        "target_session_id":{"type":"string","minLength":1},
        "checkpoint":{"type":"object","additionalProperties":false,"required":["outcome","detail","evidence"],"properties":{
            "outcome":{"type":"string","enum":["tunnel_ready","key_prepared","connector_ready","human_required"]},
            "tunnel_id":{"type":"string"},"detail":{"type":"string","maxLength":1000},
            "handoff_stage":{"type":"string","enum":["login","tunnel","credentials","connector"]},
            "organization":{"type":"string"},"project":{"type":"string"},
            "evidence":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string","maxLength":500}}
        }}
    }})
}
#[derive(Clone)]
pub(crate) struct SetupScope {
    pub run_id: String,
    pub session: SessionId,
    pub task: TaskId,
    pub turn: String,
    pub generation: i64,
    pub digest: String,
    pub snapshot: Value,
    pub catalog_cursor: u64,
    pub channel_generation: u64,
    pub cancelled: RunCancellation,
}
fn conflict(message: &str) -> ApiError {
    ApiError::Conflict(message.into())
}
fn reply(value: Result<Value, ApiError>) -> (String, ExecutionResultStatus) {
    match value {
        Ok(value) => {
            let status = if value["status"] == "indeterminate" {
                ExecutionResultStatus::Indeterminate
            } else {
                ExecutionResultStatus::Succeeded
            };
            (policy::redact(&value.to_string()), status)
        }
        Err(error) => {
            let message = match error {
                ApiError::Conflict(message)
                | ApiError::Forbidden(message)
                | ApiError::InvalidInput(message) => message,
                _ => "配置结果待核对，请先 inspect，不要重放写入".into(),
            };
            (json!({"status":"rejected","error_code":"setup_action_rejected","error":policy::redact(&message),"execution_started":false}).to_string(),ExecutionResultStatus::Rejected)
        }
    }
}
impl ApiState {
    fn setup_identity(
        &self,
        context: &ToolExecutionContext,
        write: bool,
    ) -> Result<(SessionId, TaskId, String), ApiError> {
        if context.caller_scope != ToolCallerScope::SessionMainline {
            return Err(ApiError::Forbidden("配置仅允许当前主助手操作".into()));
        }
        let session = context
            .session_id
            .clone()
            .ok_or_else(|| conflict("缺少会话身份"))?;
        let task = context
            .task_id
            .clone()
            .ok_or_else(|| conflict("缺少 Task 身份"))?;
        let current = self
            .task_store()
            .and_then(|store| store.get_task(&task))
            .ok_or_else(|| conflict("配置任务已失效"))?;
        if current.status != magi_core::TaskStatus::Running
            || (write
                && current.policy_snapshot.as_ref().is_some_and(|policy| {
                    policy
                        .denied_tools
                        .iter()
                        .any(|name| name == "browser_snapshot")
                        || policy.command_mode == "no_tools"
                }))
        {
            return Err(ApiError::Forbidden(
                "当前任务没有本机浏览器权限或已经停止".into(),
            ));
        }
        if write {
            if let Some(policy) = current.policy_snapshot.as_ref() {
                let mut scope = magi_tool_runtime::ToolExecutionPolicy::default();
                scope.apply_task_policy(policy);
                if scope.tool_name_rejection("browser_snapshot").is_some() {
                    return Err(conflict(
                        "skill_scope_conflict: 当前任务不允许 Browser 观察",
                    ));
                }
            }
            if let Some(skill) = current.executor_binding_active_skill_id()
                && self.skill_runtime.as_ref().is_some_and(|runtime| {
                    !runtime
                        .registry()
                        .is_tool_allowed(skill, "browser_snapshot")
                })
            {
                return Err(conflict(
                    "skill_scope_conflict: 原 Skill 不允许浏览器，不能通过配置扩大权限",
                ));
            }
        }
        if write
            && context
                .browser_capability_snapshot
                .as_ref()
                .is_none_or(|capability| capability.unavailable_reason().is_some())
        {
            return Err(ApiError::Forbidden("请在可信本机 Desktop 进行配置".into()));
        }
        let turn = self
            .session_store
            .runtime_sidecar(&session)
            .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id))
            .ok_or_else(|| conflict("Turn 已结束"))?;
        Ok((session, task, turn))
    }
    fn setup_digest(&self) -> Result<String, ApiError> {
        if !self.web_model_setup_available() {
            return Err(ApiError::Forbidden("请先更新并激活 GPT Web 插件".into()));
        }
        self.plugin_manager
            .lock()
            .map_err(|_| conflict("插件状态不可用"))?
            .package(magi_web_model::WEB_MODEL_PLUGIN_ID)
            .map(|package| package.digest().to_string())
            .map_err(|_| conflict("插件尚未安装"))
    }
    fn setup_version(record: &WebModelSetupStatus, args: &Value) -> Result<(), ApiError> {
        if args["expected_revision"].as_i64() != record.revision
            || args["setup_run_id"].as_str() != record.setup_run_id.as_deref()
        {
            return Err(conflict("stale_setup: 配置已变化，先 inspect 最新版本"));
        }
        Ok(())
    }
    pub fn execute_web_model_setup_tool(
        &self,
        input: &str,
        context: &ToolExecutionContext,
    ) -> (String, ExecutionResultStatus) {
        reply(crate::browser_tool_runtime::block_on(
            self.setup_tool(input, context),
        ))
    }
    async fn setup_tool(
        &self,
        input: &str,
        context: &ToolExecutionContext,
    ) -> Result<Value, ApiError> {
        let prepared = self
            .plugin_manager
            .lock()
            .map_err(|_| conflict("插件状态不可用"))?
            .prepare_tool_call(APPLICATION_SCOPE, &tool_name(), input)
            .map_err(|_| ApiError::Forbidden("配置贡献未授权或参数不符合 schema".into()))?;
        let cancel = RunCancellation::default();
        let _lease = PluginRuntimeLease::acquire(
            self.plugin_manager.clone(),
            magi_web_model::WEB_MODEL_PLUGIN_ID,
            prepared.package.digest(),
            APPLICATION_SCOPE,
            &cancel,
        )
        .map_err(|_| conflict("插件正在停用或升级"))?;
        let args = prepared.input;
        let _serial = self.web_model.setup_lock.lock().await;
        let (session, task, turn) = self.setup_identity(context, args["action"] != "inspect")?;
        if cancel.is_cancelled() {
            return Err(conflict("插件执行资格已撤销，请先 inspect"));
        }
        self.setup_digest()?;
        let action = args["action"].as_str().unwrap_or_default();
        if action == "inspect" {
            if let Some(record) = self
                .read_setup_record()
                .filter(|record| record.session_id != session.as_str() && !terminal(record))
            {
                return Ok(
                    json!({"status":"busy","ownerSessionId":record.session_id,"instruction":"请回到原会话结束或转交配置，不可抢占页面"}),
                );
            }
            return Ok(
                json!({"status":"succeeded","configuration":self.web_model_setup_status(&session),"channel":self.setup_channel_facts(),"skill":SKILL_ID}),
            );
        }
        if action == "begin" {
            let existing = self.read_setup_record();
            if let Some(record) = existing.as_ref() {
                if record.continue_state.as_deref() == Some("save_pending") {
                    return Err(conflict(
                        "凭据保存结果尚未核对，请先从安全面板查询原请求回执",
                    ));
                }
                if record.session_id != session.as_str() && !terminal(record) {
                    return Err(conflict("另一会话拥有配置维护权，请先在原会话结束配置"));
                }
                // 其他会话已结束的记录不对新拥有者公开版本，也不应阻止其新建配置。
                if record.session_id == session.as_str() {
                    Self::setup_version(record, &args)?;
                }
                if record.session_id == session.as_str()
                    && record.execution_turn_id.as_deref() == Some(turn.as_str())
                    && record.run_state.as_deref() == Some("running")
                {
                    return Ok(
                        json!({"status":"succeeded","configuration":record,"tab_id":WEB_MODEL_HOME_TAB_ID,"instruction":"当前执行已准入；不要重复 begin，先读取最新页面。"}),
                    );
                }
                if record.session_id == session.as_str()
                    && record.execution_turn_id.as_deref() == Some(turn.as_str())
                    && record.run_state.as_deref() != Some("running")
                {
                    return Err(conflict(
                        "配置已交还用户或暂停；先结束本轮响应，只有新的用户继续输入才重新准入",
                    ));
                }
            }
            let driver = self
                .web_model
                .driver()
                .ok_or_else(|| conflict("配置页面驱动不可用"))?;
            let slots = self
                .web_model
                .bindings()
                .ok_or_else(|| conflict("Desktop 浏览器尚未就绪"))?;
            slots
                .reserve_maintenance(session.as_str())
                .map_err(|_| conflict("GPT Web 聊天或其他配置占用单槽位，请先显式释放"))?;
            ensure_app_browser_session(self)?;
            let next_generation = existing
                .as_ref()
                .and_then(|record| record.execution_generation)
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| conflict("配置执行代际已耗尽"))?;
            let mut record = existing.unwrap_or_default();
            if terminal(&record) || record.setup_run_id.is_none() {
                record = WebModelSetupStatus {
                    setup_run_id: Some(magi_plugin_system::next_invocation_id("web-setup")),
                    ..Default::default()
                };
            }
            record.session_id = session.to_string();
            record.run_state = Some("running".into());
            if record.stage.is_empty()
                || matches!(record.stage.as_str(), "complete" | "cancelled" | "busy")
            {
                record.stage = "login".into();
            }
            record.reason = "observing".into();
            record.detail = None;
            record.handoff_id = None;
            record.continue_state = record
                .saved_credential_version
                .as_ref()
                .map(|_| "continued".into());
            record.continuation_input_id = None;
            record.revision = Some(record.revision.unwrap_or(0));
            record.execution_generation = Some(next_generation);
            record.execution_task_id = Some(task.to_string());
            record.execution_turn_id = Some(turn.clone());
            record.plugin_digest = Some(self.setup_digest()?);
            let channel = self.web_model.status();
            if !channel.ready
                && channel.has_api_key
                && !matches!(
                    channel.code.as_str(),
                    "client_installing" | "runtime_not_ready"
                )
                && let Some(config) = self
                    .web_model
                    .configured()
                    .filter(|config| config.is_configured())
            {
                // begin 只由本轮明确继续意图触发。重连复用已保存版本，不再次要求密钥，
                // 目录观察窗口在新通道代际创建之后才开启。
                self.web_model
                    .clone()
                    .configure(self, Some(config), None)
                    .await
                    .map_err(|_| {
                        conflict("凭据已保存，但通道重连未接纳；请核对通道状态后明确继续")
                    })?;
            }
            if cancel.is_cancelled()
                || !self
                    .setup_digest()
                    .is_ok_and(|digest| Some(digest.as_str()) == record.plugin_digest.as_deref())
            {
                slots.release_maintenance(session.as_str());
                return Err(conflict("配置准入期间插件资格已变化，请先 inspect"));
            }
            self.commit_setup_record(&mut record)?;
            let _ = self
                .browser_authority
                .lock()
                .expect("browser authority lock poisoned")
                .release_user_control(&magi_core::BrowserTabId::new(WEB_MODEL_HOME_TAB_ID));
            *self
                .web_model
                .setup_scope
                .lock()
                .expect("setup scope lock poisoned") = Some(SetupScope {
                run_id: record.setup_run_id.clone().expect("setup run initialized"),
                session,
                task,
                turn,
                generation: record.execution_generation.unwrap(),
                digest: record.plugin_digest.clone().unwrap(),
                snapshot: Value::Null,
                catalog_cursor: self.web_model.slot_catalog_cursor(),
                channel_generation: self.web_model.channel_generation(),
                cancelled: cancel.clone(),
            });
            // 仅恢复物理页面；恢复失败不导航并覆盖用户尚未确认的表单。
            let refreshed = driver.refresh_surface(WEB_MODEL_HOME_TAB_ID).await;
            if refreshed.is_err() || cancel.is_cancelled() || self.setup_digest().is_err() {
                self.close_setup_scope(
                    &SessionId::new(&record.session_id),
                    record.execution_generation,
                )
                .await?;
                record.run_state = Some("blocked".into());
                record.reason = "surface_unavailable".into();
                self.commit_setup_record(&mut record)?;
                return Err(conflict(
                    "配置页面尚未就绪，已停止页面执行，请核对 Browser 后明确继续",
                ));
            }
            return Ok(
                json!({"status":"succeeded","configuration":record,"channel":self.setup_channel_facts(),"tab_id":WEB_MODEL_HOME_TAB_ID,"instruction":"配置 scope 已绑定当前执行。使用 browser_snapshot 观察；每次写入后重新观察。需要人工时 checkpoint 并结束本轮响应；普通讨论不能自动继续。"}),
            );
        }
        let mut record = self
            .web_model_setup_status(&session)
            .ok_or_else(|| conflict("配置不属于当前会话"))?;
        Self::setup_version(&record, &args)?;
        if terminal(&record) {
            return if action == "cancel" && record.run_state.as_deref() == Some("cancelled") {
                Ok(json!({"status":"succeeded","configuration":record}))
            } else {
                Err(conflict(
                    "配置已结束，旧执行不能修改历史结果；需要新的用户输入重新开始",
                ))
            };
        }
        if action == "transfer" {
            if record.continue_state.as_deref() == Some("save_pending") {
                return Err(conflict("凭据保存结果尚未核对，请先查询原请求回执再转交"));
            }
            let target = SessionId::new(
                args["target_session_id"]
                    .as_str()
                    .ok_or_else(|| conflict("请明确目标会话"))?,
            );
            if target == session || self.session_store.session(&target).is_none() {
                return Err(conflict("目标会话不存在或就是当前会话"));
            }
            self.close_setup_scope(&session, None).await?;
            self.invalidate_setup_continuation(&session, None)?;
            let slots = self
                .web_model
                .bindings()
                .ok_or_else(|| conflict("配置维护资源不可用"))?;
            slots
                .transfer_maintenance(session.as_str(), target.as_str())
                .map_err(|_| conflict("配置归属已变化，转交未执行"))?;
            record.session_id = target.to_string();
            record.revision = Some(record.revision.unwrap_or(0) + 1);
            record.execution_generation = Some(record.execution_generation.unwrap_or(0) + 1);
            record.execution_task_id = None;
            record.execution_turn_id = None;
            record.run_state = Some("paused".into());
            record.reason = "transferred".into();
            record.handoff_id = None;
            record.continue_state = None;
            record.continuation_input_id = None;
            record.save_request_id = None;
            record.save_request_binding = None;
            if let Err(error) = self.commit_setup_record(&mut record) {
                let _ = slots.transfer_maintenance(target.as_str(), session.as_str());
                return Err(error);
            }
            return Ok(
                json!({"status":"succeeded","configuration":record,"instruction":"配置已暂停并转交；目标会话明确继续后才能取得页面控制。旧表单和执行身份已失效。"}),
            );
        }
        if matches!(action, "pause" | "cancel") {
            self.close_setup_scope(&session, None).await?;
            record.run_state = Some(
                if action == "cancel" {
                    "cancelled"
                } else {
                    "paused"
                }
                .into(),
            );
            record.handoff_id = None;
            record.continue_state = None;
            if action == "cancel" {
                record.stage = "cancelled".into();
                if let Some(slots) = self.web_model.bindings() {
                    slots.release_maintenance(session.as_str());
                }
            }
            self.commit_setup_record(&mut record)?;
            return Ok(json!({"status":"succeeded","configuration":record}));
        }
        let scope = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .clone()
            .filter(|scope| scope.session == session && scope.task == task && scope.turn == turn)
            .ok_or_else(|| conflict("配置 scope 已关闭，用户继续后重新 begin"))?;
        if record.setup_run_id.as_deref() != Some(scope.run_id.as_str())
            || record.execution_generation != Some(scope.generation)
            || self.setup_digest()? != scope.digest
            || scope.cancelled.is_cancelled()
        {
            return Err(conflict("配置运行、插件版本或执行资格已失效，请先 inspect"));
        }
        let checkpoint: policy::Checkpoint = serde_json::from_value(args["checkpoint"].clone())
            .map_err(|_| ApiError::InvalidInput("缺少有效 checkpoint".into()))?;
        self.validate_setup_snapshot(&scope.snapshot)?;
        policy::validate_checkpoint(&checkpoint, &scope.snapshot)
            .map_err(ApiError::InvalidInput)?;
        record.organization = checkpoint.organization;
        record.project = checkpoint.project;
        record.detail = Some(policy::redact(&checkpoint.detail));
        match checkpoint.outcome.as_str() {
            "tunnel_ready" => {
                let id = checkpoint
                    .tunnel_id
                    .ok_or_else(|| conflict("缺少 Tunnel ID"))?;
                let mut config = self.web_model.configured().unwrap_or_default();
                if !config.tunnel_id.is_empty() && config.tunnel_id != id {
                    return Err(conflict(
                        "页面 Tunnel 与保存配置不同，请在安全表单核对后更换",
                    ));
                }
                config.tunnel_id = id;
                self.settings_store
                    .set_section(crate::WEB_MODEL_TUNNEL_SECTION, json!(config))
                    .map_err(crate::errors::settings_persistence_error)?;
                self.web_model.set_setup_config(config);
                record.stage = "credentials".into();
                record.reason = "observing".into();
                record.revision = Some(record.revision.unwrap_or(0) + 1);
            }
            "key_prepared" | "human_required" => {
                self.close_setup_scope(&session, Some(scope.generation))
                    .await?;
                record.run_state = Some("waiting_user".into());
                record.handoff_id = Some(magi_plugin_system::next_invocation_id("setup-handoff"));
                if checkpoint.outcome == "key_prepared" {
                    record.stage = "credentials".into();
                    record.reason = "key_generation_required".into();
                } else {
                    record.stage = checkpoint
                        .handoff_stage
                        .ok_or_else(|| conflict("人工交接必须指定阶段"))?;
                    record.reason = "human_required".into();
                }
            }
            "connector_ready" => {
                if !self.web_model.status().ready
                    || scope.channel_generation != self.web_model.channel_generation()
                    || !self
                        .web_model
                        .slot_catalog_served_after(scope.channel_generation, scope.catalog_cursor)
                {
                    return Err(conflict("当前通道或本次工具目录读取证据不足，请先核验"));
                }
                self.close_setup_scope(&session, Some(scope.generation))
                    .await?;
                // 当前 stdio 槽位协议只有共享 principal，没有可信连接器来源。
                // 页面文案 + 本机 tools/list 也可能由探针或另一客户端产生，不能宣布完成。
                record.stage = "connector".into();
                record.run_state = Some("blocked".into());
                record.reason = "connector_attribution_pending".into();
                record.detail = Some("通道可达，已观察到工具目录读取；当前传输无法确认请求来自目标 ChatGPT 连接器，配置尚未完成。请在 ChatGPT 中核对连接器并验证实际工具往返。".into());
                record.handoff_id = None;
            }
            _ => return Err(ApiError::InvalidInput("无效检查点".into())),
        }
        self.commit_setup_record(&mut record)?;
        Ok(
            json!({"status":"succeeded","configuration":record,"channel":self.setup_channel_facts(),"phase_delivered":record.run_state.as_deref()==Some("waiting_user"),"instruction":"准确解释阶段交付和用户下一步；人工交接不等于整个配置或原任务目标完成。"}),
        )
    }
    fn setup_channel_facts(&self) -> Value {
        let status = self.web_model.status();
        json!({"tunnelId":status.tunnel_id,"hasApiKey":status.has_api_key,"ready":status.ready,"code":status.code})
    }
    pub(crate) async fn close_setup_scope(
        &self,
        session: &SessionId,
        generation: Option<i64>,
    ) -> Result<(), ApiError> {
        let matches = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .as_ref()
            .is_some_and(|scope| {
                &scope.session == session
                    && generation.is_none_or(|generation| generation == scope.generation)
            });
        if matches {
            // 先撤销克隆身份的执行资格；物理租约释放失败时也不能重试页面动作。
            if let Some(scope) = self
                .web_model
                .setup_scope
                .lock()
                .expect("setup scope lock poisoned")
                .as_ref()
            {
                scope.cancelled.cancel();
            }
            if let Some(driver) = self.web_model.driver() {
                driver
                    .end_setup_control()
                    .await
                    .map_err(crate::web_model_ops::web_model_api_error)?;
            }
            *self
                .web_model
                .setup_scope
                .lock()
                .expect("setup scope lock poisoned") = None;
        }
        Ok(())
    }
    pub fn execute_setup_browser(
        &self,
        name: &str,
        input: &str,
        context: &ToolExecutionContext,
    ) -> Option<(String, ExecutionResultStatus)> {
        let scope = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .clone()?;
        if context.session_id.as_ref() != Some(&scope.session)
            || context.task_id.as_ref() != Some(&scope.task)
        {
            return None;
        }
        Some(reply(crate::browser_tool_runtime::block_on(async {
            let _serial = self.web_model.setup_lock.lock().await;
            let (_, _, turn) = self.setup_identity(context, true)?;
            if turn != scope.turn || self.setup_digest()? != scope.digest {
                return Err(conflict("旧配置执行已失效"));
            }
            if scope.cancelled.is_cancelled() {
                return Err(conflict("配置已停止，不能领取新动作"));
            }
            let current = self
                .web_model
                .setup_scope
                .lock()
                .expect("setup scope lock poisoned")
                .clone()
                .filter(|current| {
                    current.run_id == scope.run_id
                        && current.task == scope.task
                        && current.generation == scope.generation
                })
                .ok_or_else(|| conflict("配置 scope 已关闭"))?;
            let _lease = PluginRuntimeLease::acquire(
                self.plugin_manager.clone(),
                magi_web_model::WEB_MODEL_PLUGIN_ID,
                &scope.digest,
                APPLICATION_SCOPE,
                &scope.cancelled,
            )
            .map_err(|_| conflict("插件正在停用"))?;
            let mut args: Value = serde_json::from_str(input)
                .map_err(|_| ApiError::InvalidInput("参数不是 JSON".into()))?;
            if let Some(tab) = args.get("tab_id")
                && tab.as_str() != Some(WEB_MODEL_HOME_TAB_ID)
            {
                return Err(ApiError::Forbidden("配置 scope 不能访问其他 Tab".into()));
            }
            if let Some(object) = args.as_object_mut() {
                object.remove("tab_id");
            }
            let kind = magi_browser_authority::BrowserToolKind::from_name(name)
                .ok_or_else(|| conflict("未知 Browser 入口"))?;
            if !jsonschema::is_valid(&kind.input_schema(), &args) {
                return Err(ApiError::InvalidInput(
                    "Browser 参数不符合共享 schema".into(),
                ));
            }
            if policy::contains_secret(&args.to_string()) {
                return Err(ApiError::Forbidden("密钥只能由安全表单提交".into()));
            }
            policy::validate_action(name, &args, &current.snapshot)
                .map_err(ApiError::InvalidInput)?;
            if name != "browser_snapshot" && name != "browser_navigate" {
                self.validate_setup_snapshot(&current.snapshot)?;
            }
            let driver = self
                .web_model
                .driver()
                .ok_or_else(|| conflict("配置驱动不可用"))?;
            if !matches!(
                name,
                "browser_snapshot" | "browser_read" | "browser_wait_for"
            ) {
                self.mark_setup_write(&scope.session)?;
            }
            let outcome = crate::web_model_driver::with_web_setup_turn(
                scope.turn.clone(),
                scope.cancelled.clone(),
                async {
                    let mut value = driver.setup_browser(name, &args).await?;
                    if scope.cancelled.is_cancelled() {
                        return Err(magi_web_model::WebModelError::cancelled());
                    }
                    if name != "browser_snapshot"
                        && (args["include_snapshot"] == true
                            || value["outcome"]["result"] == "page_navigated")
                    {
                        value["snapshot"] =
                            driver.setup_browser("browser_snapshot", &json!({})).await?;
                    }
                    Ok::<_, magi_web_model::WebModelError>(value)
                },
            )
            .await;
            // 已送达的动作先等待回执结算；撤权后不发布页面成功或继续采集快照。
            let outcome = outcome.and_then(|value| {
                if scope.cancelled.is_cancelled()
                    || self.setup_digest().is_err()
                    || self.setup_identity(context, true).is_err()
                {
                    Err(magi_web_model::WebModelError::cancelled())
                } else {
                    Ok(value)
                }
            });
            match outcome {
                Ok(value) => {
                    let value = policy::redact_value(value);
                    let response_url = if name == "browser_snapshot" || name == "browser_read" {
                        value.get("url")
                    } else {
                        value
                            .get("snapshot")
                            .and_then(|snapshot| snapshot.get("url"))
                            .or_else(|| value.get("page").and_then(|page| page.get("url")))
                    };
                    if response_url
                        .and_then(Value::as_str)
                        .is_some_and(|url| !policy::allowed_origin(url))
                    {
                        self.close_setup_scope(&scope.session, Some(scope.generation))
                            .await?;
                        if let Some(mut record) = self.read_setup_record() {
                            record.run_state = Some("waiting_user".into());
                            record.stage = "login".into();
                            record.reason = "external_login_required".into();
                            record.handoff_id =
                                Some(magi_plugin_system::next_invocation_id("setup-handoff"));
                            self.commit_setup_record(&mut record)?;
                        }
                        return Err(ApiError::Forbidden(
                            "外部登录需要用户操作，已交还页面控制".into(),
                        ));
                    }
                    if let Some(active) = self
                        .web_model
                        .setup_scope
                        .lock()
                        .expect("setup scope lock poisoned")
                        .as_mut()
                    {
                        if name == "browser_snapshot" {
                            active.snapshot = value.clone();
                        } else if let Some(snapshot) = value.get("snapshot") {
                            active.snapshot = snapshot.clone();
                        } else if !matches!(name, "browser_read" | "browser_wait_for") {
                            active.snapshot = Value::Null;
                        }
                    }
                    Ok(json!({"status":"succeeded","untrusted_web_content":true,"result":value}))
                }
                Err(error) => {
                    tracing::warn!(tool = name, reason = %policy::redact(&error.message), "配置页面动作未确认，停止自动执行");
                    let takeover = error.message.contains("browser_user_control_held");
                    self.close_setup_scope(&scope.session, Some(scope.generation))
                        .await?;
                    if let Some(mut record) = self.read_setup_record() {
                        record.run_state = Some(if takeover { "paused" } else { "blocked" }.into());
                        record.reason = if takeover {
                            "user_takeover"
                        } else {
                            "result_unconfirmed"
                        }
                        .into();
                        record.detail = Some(
                            if takeover {
                                "你已接管页面。完成后明确继续，Magi 会先重新观察页面。"
                            } else {
                                "页面结果待核对；请明确继续后先观察，不重放写入"
                            }
                            .into(),
                        );
                        self.commit_setup_record(&mut record)?;
                    }
                    Ok(
                        json!({"status":"indeterminate","error_code":"setup_result_unconfirmed","execution_started":true,"error":"页面结果待核对，已停止自动操作；先 inspect 最新配置，再由用户明确继续。","configuration":self.web_model_setup_status(&scope.session)}),
                    )
                }
            }
        })))
    }
    /// Browser Authority 已确认的接管立即撤销新动作资格；结算后发布配置暂停事实。
    pub fn notify_web_model_setup_takeover(&self, tab: &magi_core::BrowserTabId) {
        if tab.as_str() != WEB_MODEL_HOME_TAB_ID {
            return;
        }
        let scope = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .clone();
        let Some(scope) = scope else {
            return;
        };
        scope.cancelled.cancel();
        let state = self.clone();
        tokio::spawn(async move {
            let _serial = state.web_model.setup_lock.lock().await;
            let Some(mut record) = state.read_setup_record().filter(|record| {
                record.session_id == scope.session.as_str()
                    && record.setup_run_id.as_deref() == Some(scope.run_id.as_str())
                    && record.execution_generation == Some(scope.generation)
                    && record.run_state.as_deref() == Some("running")
            }) else {
                return;
            };
            if state
                .close_setup_scope(&scope.session, Some(scope.generation))
                .await
                .is_err()
            {
                return;
            }
            record.run_state = Some("paused".into());
            record.reason = "user_takeover".into();
            record.detail = Some("你已接管页面。完成后明确继续，Magi 会先重新观察页面。".into());
            record.handoff_id = None;
            let _ = state.commit_setup_record(&mut record);
        });
    }
    pub fn finish_web_model_setup_task(&self, task: &TaskId) {
        let scope = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .clone()
            .filter(|scope| &scope.task == task);
        if let Some(scope) = scope {
            let _ = crate::browser_tool_runtime::block_on(async {
                let _serial = self.web_model.setup_lock.lock().await;
                self.close_setup_scope(&scope.session, Some(scope.generation))
                    .await?;
                if let Some(mut record) = self.read_setup_record().filter(|record| {
                    record.execution_generation == Some(scope.generation)
                        && record.setup_run_id.as_deref() == Some(scope.run_id.as_str())
                        && record.execution_task_id.as_deref() == Some(task.as_str())
                }) && record.run_state.as_deref() == Some("running")
                {
                    record.run_state = Some("paused".into());
                    record.reason = "execution_ended".into();
                    self.commit_setup_record(&mut record)?;
                }
                Ok::<_, ApiError>(())
            });
        }
    }
    fn validate_setup_snapshot(&self, snapshot: &Value) -> Result<(), ApiError> {
        let navigation = snapshot["navigation_revision"]
            .as_u64()
            .ok_or_else(|| conflict("请先获取当前页面快照"))?;
        let revision = snapshot["snapshot_revision"]
            .as_u64()
            .ok_or_else(|| conflict("当前快照缺少版本，请重新观察"))?;
        self.browser_authority
            .lock()
            .expect("browser authority lock poisoned")
            .validate_snapshot_result(
                &magi_core::BrowserTabId::new(WEB_MODEL_HOME_TAB_ID),
                navigation,
                revision,
            )
            .map_err(|_| conflict("页面或快照已失效，旧节点和证据不可继续使用"))
    }
}
fn terminal(record: &WebModelSetupStatus) -> bool {
    matches!(
        record.run_state.as_deref(),
        Some("completed" | "cancelled" | "revoked")
    ) || matches!(record.stage.as_str(), "complete" | "cancelled")
}
