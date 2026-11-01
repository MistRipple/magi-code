//! GPT Web 配置持久事实与安全表单。执行由主对话及 setup_tools 统一驱动。
use crate::{ApiError, ApiState};
use magi_app_server_protocol::generated::WebModelSetupStatus;
use magi_core::{SessionId, UtcMillis};
use serde_json::{Value, json};
pub const SETUP_COMMAND_ID: &str = "plugin/openai.chatgpt-web/setup";
const SETUP_SECTION: &str = "webModelSetup";
const RECEIPTS_SECTION: &str = "webModelSetupReceipts";
impl ApiState {
    pub(crate) fn read_setup_record(&self) -> Option<WebModelSetupStatus> {
        serde_json::from_value(self.settings_store.get_section(SETUP_SECTION)).ok()
    }
    pub(crate) fn web_model_setup_status(
        &self,
        session: &SessionId,
    ) -> Option<WebModelSetupStatus> {
        self.read_setup_record()
            .filter(|record| record.session_id == session.as_str())
            .map(|mut record| {
                if record.run_state.is_none() {
                    record.run_state = Some(
                        match record.stage.as_str() {
                            "complete" => "completed",
                            "cancelled" => "cancelled",
                            _ => "paused",
                        }
                        .into(),
                    );
                }
                record.allowed_actions = Some(if !self.web_model_setup_available() {
                    vec![]
                } else {
                    match record.run_state.as_deref() {
                        Some("running") => vec!["pause".into(), "cancel".into()],
                        Some("completed" | "cancelled" | "revoked") => vec!["start".into()],
                        _ if record.continue_state.as_deref() == Some("save_pending") => {
                            vec!["cancel".into()]
                        }
                        _ => {
                            let mut actions = vec!["continue".into(), "cancel".into()];
                            if record.stage == "credentials" && record.handoff_id.is_some() {
                                actions.push("save_credentials".into());
                            }
                            actions
                        }
                    }
                });
                record
            })
    }
    pub(crate) fn commit_setup_record(
        &self,
        record: &mut WebModelSetupStatus,
    ) -> Result<(), ApiError> {
        let previous = self.read_setup_record();
        record.updated_at = (UtcMillis::now().0 as i64).max(
            previous
                .as_ref()
                .map_or(0, |previous| previous.updated_at.saturating_add(1)),
        );
        record.allowed_actions = None;
        if let Some(previous) = previous.filter(|previous| {
            previous.save_request_id.is_some()
                && (previous.session_id != record.session_id
                    || previous.setup_run_id != record.setup_run_id
                    || previous.save_request_id != record.save_request_id)
        }) {
            // 请求被新交接/运行替换时，原回执与替换提交同一 Settings 事务。
            // 历史回执没有执行权，也不参与当前配置事实的派生。
            let session = SessionId::new(&previous.session_id);
            let mut receipts = self
                .settings_store
                .get_session_section(&session, RECEIPTS_SECTION)
                .as_object()
                .cloned()
                .unwrap_or_default();
            receipts.insert(
                previous.save_request_id.clone().expect("request checked"),
                json!(previous),
            );
            self.settings_store
                .set_session_and_global_sections(
                    &session,
                    RECEIPTS_SECTION,
                    Value::Object(receipts),
                    SETUP_SECTION,
                    json!(record),
                )
                .map_err(crate::errors::settings_persistence_error)?;
        } else {
            self.settings_store
                .set_section(SETUP_SECTION, json!(record))
                .map_err(crate::errors::settings_persistence_error)?;
        }
        self.event_bus.publish(
            magi_event_bus::EventEnvelope::projection(
                magi_core::EventId::unique("web-setup-updated"),
                "web_setup.updated",
                json!({"setupRunId":record.setup_run_id,"revision":record.revision}),
            )
            .with_context(magi_event_bus::EventContext {
                session_id: Some(SessionId::new(&record.session_id)),
                ..Default::default()
            }),
        );
        Ok(())
    }
    pub(crate) fn mark_setup_write(&self, session: &SessionId) -> Result<(), ApiError> {
        if let Some(mut record) = self.web_model_setup_status(session) {
            record.reason = "write_pending".into();
            self.commit_setup_record(&mut record)?;
        }
        Ok(())
    }
    pub fn restore_web_model_setup_reservation(&self) {
        if let Some(mut record) = self.read_setup_record() {
            if matches!(
                record.run_state.as_deref(),
                Some("completed" | "cancelled" | "revoked")
            ) || matches!(record.stage.as_str(), "complete" | "cancelled")
            {
                return;
            }
            self.reconcile_setup_save(&mut record);
            if let Some(input) = record.continuation_input_id.as_ref() {
                let _ =
                    self.remove_regular_session_turn(&SessionId::new(&record.session_id), input);
            }
            record.handoff_id = None;
            record.continue_state = if record.continue_state.as_deref() == Some("save_pending") {
                Some("save_pending".into())
            } else {
                record
                    .saved_credential_version
                    .as_ref()
                    .map(|_| "saved_not_continued".into())
            };
            record.continuation_input_id = None;
            if !matches!(
                record.run_state.as_deref(),
                Some("completed" | "cancelled" | "revoked")
            ) && !matches!(record.stage.as_str(), "complete" | "cancelled")
            {
                record.run_state = Some("paused".into());
                record.reason = "restart_requires_continue".into();
                record.execution_generation = Some(record.execution_generation.unwrap_or(0) + 1);
                if self.web_model_setup_available()
                    && let Some(slots) = self.web_model.bindings()
                {
                    let _ = slots.reserve_maintenance(&record.session_id);
                }
            }
            let _ = self.commit_setup_record(&mut record);
        }
    }
    pub(crate) async fn cancel_web_model_setup_for(&self, session: Option<&SessionId>) {
        if let Some(scope) = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .as_ref()
            .filter(|scope| session.is_none_or(|session| session == &scope.session))
        {
            scope.cancelled.cancel();
        }
        let _serial = self.web_model.setup_lock.lock().await;
        if let Some(mut record) = self.read_setup_record() {
            if matches!(
                record.run_state.as_deref(),
                Some("completed" | "cancelled" | "revoked")
            ) || matches!(record.stage.as_str(), "complete" | "cancelled")
            {
                // 历史完成与当前健康分开。停用/停止不得把已完成记录改写成取消。
                return;
            }
            if session.is_some_and(|session| session.as_str() != record.session_id) {
                return;
            }
            self.reconcile_setup_save(&mut record);
            let owner = SessionId::new(&record.session_id);
            let _ = self.invalidate_setup_continuation(&owner, None);
            if self.close_setup_scope(&owner, None).await.is_err() {
                return;
            }
            record.run_state = Some(
                if self.web_model_setup_available() {
                    "cancelled"
                } else {
                    "revoked"
                }
                .into(),
            );
            record.handoff_id = None;
            record.continue_state = None;
            let _ = self.commit_setup_record(&mut record);
            if let Some(slots) = self.web_model.bindings() {
                slots.release_maintenance(owner.as_str());
            }
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SetupContinuation {
    pub runtime_epoch: String,
    pub setup_run_id: String,
    pub revision: i64,
    pub plugin_digest: String,
    pub channel_generation: u64,
    pub deadline: u64,
}
impl ApiState {
    pub(crate) fn setup_continuation_ready(&self, condition: &SetupContinuation) -> bool {
        condition.runtime_epoch == self.runtime_epoch()
            && UtcMillis::now().0 < condition.deadline
            && self.web_model_setup_available()
            && self.plugin_manager.lock().ok().is_some_and(|manager| {
                manager
                    .package(magi_web_model::WEB_MODEL_PLUGIN_ID)
                    .is_ok_and(|package| package.digest() == condition.plugin_digest)
            })
            && self.web_model.channel_generation() == condition.channel_generation
            && self.read_setup_record().is_some_and(|record| {
                record.setup_run_id.as_deref() == Some(condition.setup_run_id.as_str())
                    && record.revision == Some(condition.revision)
                    && record.plugin_digest.as_deref() == Some(condition.plugin_digest.as_str())
                    && record.continue_state.as_deref() == Some("waiting_channel")
            })
            && self.web_model.status().ready
    }
    fn reconcile_setup_save(&self, record: &mut WebModelSetupStatus) {
        if record.continue_state.as_deref() == Some("waiting_channel")
            && record
                .continuation_input_id
                .as_ref()
                .and_then(|id| {
                    self.session_store
                        .input_for_id(&SessionId::new(&record.session_id), id)
                })
                .is_some_and(|input| {
                    matches!(
                        input.state,
                        magi_session_store::SessionInputState::Attached
                            | magi_session_store::SessionInputState::Consumed
                    )
                })
        {
            record.continue_state = Some("continued".into());
            record.continuation_input_id = None;
        }
        if record.continue_state.as_deref() != Some("save_pending") {
            return;
        }
        let committed = serde_json::from_value::<crate::web_model_channel::WebModelTunnelConfig>(
            self.settings_store
                .get_section(crate::WEB_MODEL_TUNNEL_SECTION),
        )
        .ok();
        if record.pending_credential_version.is_some()
            && committed
                .as_ref()
                .and_then(|config| config.credential_version.as_ref())
                == record.pending_credential_version.as_ref()
        {
            record.saved_credential_version = record.pending_credential_version.take();
            // 私有配置已经提交，但 setup 回执可能在 revision 提交前失败。
            // 与正常保存使用相同版本边界；多次查询不会重复递增。
            record.revision = Some(record.revision.unwrap_or(0) + 1);
            record.continue_state = Some("saved_not_continued".into());
            record.handoff_id = None;
            record.stage = "connecting".into();
            record.run_state = Some("paused".into());
        }
    }
    pub(crate) fn setup_save_receipt(
        &self,
        session: &SessionId,
        request_id: &str,
    ) -> Result<Value, ApiError> {
        let mut record = self
            .setup_save_record_for_request(session, request_id)
            .ok_or_else(|| ApiError::NotFound("没有该安全请求的回执，请先同步配置状态".into()))?;
        let previous = record.continue_state.clone();
        let is_current = self.read_setup_record().is_some_and(|current| {
            current.session_id == session.as_str()
                && current.setup_run_id == record.setup_run_id
                && current.save_request_id.as_deref() == Some(request_id)
        });
        if is_current {
            self.reconcile_setup_save(&mut record);
        }
        if is_current && previous != record.continue_state {
            self.commit_setup_record(&mut record)?;
        }
        Ok(
            json!({"saved":record.saved_credential_version.is_some(),"status":record.continue_state,"configuration":record}),
        )
    }
    fn setup_save_record_for_request(
        &self,
        session: &SessionId,
        request_id: &str,
    ) -> Option<WebModelSetupStatus> {
        self.read_setup_record()
            .filter(|record| {
                record.session_id == session.as_str()
                    && record.save_request_id.as_deref() == Some(request_id)
            })
            .or_else(|| {
                let receipts = self
                    .settings_store
                    .get_session_section(session, RECEIPTS_SECTION);
                serde_json::from_value::<WebModelSetupStatus>(receipts.get(request_id)?.clone())
                    .ok()
                    .filter(|record| {
                        record.session_id == session.as_str()
                            && record.save_request_id.as_deref() == Some(request_id)
                    })
            })
    }
    pub(crate) async fn save_setup_credentials_request(
        &self,
        request: magi_app_server_protocol::generated::WebModelSetupCredentialsParams,
    ) -> Result<Value, ApiError> {
        let session = SessionId::new(&request.session_id);
        let _turn = self.lock_session_turn_commit(&session).await;
        let _serial = self.web_model.setup_lock.lock().await;
        if request.request_id.is_empty() || request.request_id.len() > 192 {
            return Err(ApiError::InvalidInput("安全请求身份无效".into()));
        }
        let binding = serde_json::to_string(&json!([
            request.setup_run_id,
            request.expected_revision,
            request.handoff_id,
            request.tunnel_id.trim(),
            request.continue_after_save.unwrap_or(false)
        ]))
        .expect("binding serializes");
        // 安全请求重试只返回原回执，不回显或再次覆盖秘密。
        if let Some(receipt) = self.setup_save_record_for_request(&session, &request.request_id) {
            if receipt.save_request_binding.as_deref() != Some(binding.as_str()) {
                return Err(ApiError::Conflict(
                    "request_conflict: 同一请求身份不能更换目标或交接".into(),
                ));
            }
            return self.setup_save_receipt(&session, &request.request_id);
        }
        if !self.web_model_setup_available() {
            return Err(ApiError::Forbidden(
                "请先激活支持主对话配置的 GPT Web 插件".into(),
            ));
        }
        let mut record = self
            .web_model_setup_status(&session)
            .ok_or_else(|| ApiError::Conflict("当前会话没有配置交接".into()))?;
        if record.continue_state.as_deref() == Some("save_pending") {
            return Err(ApiError::Conflict(
                "保存结果尚未核对，请查询原请求回执，不能覆盖新的秘密".into(),
            ));
        }
        if record.setup_run_id.as_deref() != Some(request.setup_run_id.as_str())
            || record.revision != Some(request.expected_revision)
            || record.handoff_id.as_deref() != Some(request.handoff_id.as_str())
            || record.run_state.as_deref() != Some("waiting_user")
            || record.stage != "credentials"
        {
            return Err(ApiError::Conflict(
                "stale_handoff: 交接或作用域已变化，请刷新后核对，旧密钥不能套到新配置".into(),
            ));
        }
        if self
            .web_model
            .bindings()
            .and_then(|slots| slots.maintenance_owner())
            .as_deref()
            != Some(session.as_str())
        {
            return Err(ApiError::Conflict("配置归属已变化".into()));
        }
        let digest = self
            .plugin_manager
            .lock()
            .map_err(|_| ApiError::Conflict("插件状态不可用".into()))?
            .package(magi_web_model::WEB_MODEL_PLUGIN_ID)
            .map_err(|_| ApiError::Forbidden("插件未安装".into()))?
            .digest()
            .to_string();
        let cancellation = magi_plugin_system::RunCancellation::default();
        let _lease = magi_plugin_system::PluginRuntimeLease::acquire(
            self.plugin_manager.clone(),
            magi_web_model::WEB_MODEL_PLUGIN_ID,
            &digest,
            magi_plugin_system::APPLICATION_SCOPE,
            &cancellation,
        )
        .map_err(|_| ApiError::Conflict("插件正在停用".into()))?;
        if record.plugin_digest.as_deref() != Some(&digest) {
            return Err(ApiError::Conflict("插件版本已变化，请重新交接".into()));
        }
        let mut config = self.web_model.configured().unwrap_or_default();
        if !magi_web_model::tunnel::valid_tunnel_id(request.tunnel_id.trim()) {
            return Err(ApiError::InvalidInput("Tunnel ID 格式不正确".into()));
        }
        if config.tunnel_id != request.tunnel_id.trim() && request.api_key.trim().is_empty() {
            return Err(ApiError::InvalidInput(
                "更换 Tunnel 时必须提交对应密钥".into(),
            ));
        }
        if !request.api_key.trim().is_empty() {
            crate::web_model_channel::validate_api_key(&request.api_key)
                .map_err(ApiError::InvalidInput)?;
        }
        config.tunnel_id = request.tunnel_id.trim().into();
        record.save_request_id = Some(request.request_id.clone());
        record.save_request_binding = Some(binding);
        record.saved_credential_version = None;
        record.pending_credential_version =
            Some(magi_plugin_system::next_invocation_id("credential"));
        record.continue_state = Some("save_pending".into());
        self.commit_setup_record(&mut record)?;
        let config = self
            .web_model
            .persist_credentials(
                self,
                config,
                Some(&request.api_key),
                record.pending_credential_version.clone(),
            )
            .map_err(|_| {
                ApiError::Conflict("保存结果待核对，请查询原请求回执，不要再次粘贴密钥".into())
            })?;
        record.saved_credential_version = record.pending_credential_version.take();
        record.revision = Some(record.revision.unwrap_or(0) + 1);
        record.handoff_id = None;
        record.stage = "connecting".into();
        record.reason = "waiting_channel".into();
        record.detail = None;
        record.run_state = Some("paused".into());
        record.continue_state = Some("saved_not_continued".into());
        self.commit_setup_record(&mut record)?;
        if self
            .web_model
            .clone()
            .configure(self, Some(config), None)
            .await
            .is_err()
        {
            record.run_state = Some("blocked".into());
            record.reason = "channel_failed".into();
            self.commit_setup_record(&mut record)?;
            return Ok(json!({"saved":true,"status":"saved_not_continued","configuration":record}));
        }
        if request.continue_after_save == Some(true) {
            let input_id = format!("setup-continue-{}", request.request_id);
            match self.enqueue_setup_continuation(&session, &record, &input_id) {
                Ok(()) => {
                    record.continuation_input_id = Some(input_id);
                    record.continue_state = Some("waiting_channel".into());
                    self.commit_setup_record(&mut record)?;
                }
                Err(_) => {
                    return Ok(
                        json!({"saved":true,"status":"saved_not_continued","configuration":record}),
                    );
                }
            }
        }
        let response = json!({"saved":true,"status":record.continue_state,"configuration":record});
        drop(_serial);
        drop(_turn);
        crate::turn_service::schedule_next_queued_regular_session_turn(self.clone(), session, None);
        Ok(response)
    }
    fn enqueue_setup_continuation(
        &self,
        session: &SessionId,
        record: &WebModelSetupStatus,
        input_id: &str,
    ) -> Result<(), ApiError> {
        let source = record
            .execution_task_id
            .as_ref()
            .and_then(|id| {
                self.task_store()
                    .and_then(|store| store.get_task(&magi_core::TaskId::new(id)))
            })
            .ok_or_else(|| {
                ApiError::Conflict("原任务权限绑定不可用；凭据已保存，请在主对话明确继续".into())
            })?;
        if source.policy_snapshot.as_ref().is_some_and(|policy| {
            !policy.allowed_tools.is_empty()
                || policy.command_mode == "no_tools"
                || policy.denied_tools.iter().any(|tool| {
                    !matches!(tool.as_str(), "get_goal" | "create_goal" | "update_goal")
                })
        }) {
            return Err(ApiError::Conflict(
                "原任务有额外工具限制，凭据已保存，请明确发起下一轮继续".into(),
            ));
        }
        let workspace = self
            .session_store
            .session(session)
            .and_then(|session| self.session_workspace_id(&session));
        let mut request:crate::dto::SessionTurnRequestDto=serde_json::from_value(json!({
            "sessionId":session,"scope":if workspace.is_some(){"workspace"}else{"personal"},"workspaceId":workspace,"requestId":input_id,"userMessageId":format!("{input_id}-user"),
            "text":"我已通过安全表单保存凭据，请在通道就绪后继续 GPT Web 配置。先核验当前状态和页面，不重放未知操作。",
            "skillName":null,"accessProfile":source.policy_snapshot.as_ref().map(magi_core::TaskPolicy::effective_access_profile).unwrap_or_default()
        })).map_err(|error|ApiError::internal_assembly("创建继续输入失败",error))?;
        request.desktop_browser_tools_allowed = true;
        request.skill_name = source
            .executor_binding_active_skill_id()
            .map(str::to_string);
        request.project_directories = source
            .executor_binding
            .as_ref()
            .and_then(|binding| binding.project_directories.clone());
        let fingerprint = request
            .request_fingerprint()
            .map_err(ApiError::InvalidInput)?;
        self.enqueue_regular_session_turn(crate::state::QueuedRegularSessionTurn {
            readiness: Some(SetupContinuation {
                runtime_epoch: self.runtime_epoch().into(),
                setup_run_id: record.setup_run_id.clone().unwrap_or_default(),
                revision: record.revision.unwrap_or(0),
                plugin_digest: record.plugin_digest.clone().unwrap_or_default(),
                channel_generation: self.web_model.channel_generation(),
                deadline: UtcMillis::now().0 + 180_000,
            }),
            project_directories: request.project_directories.clone(),
            request,
            request_fingerprint: Some(fingerprint),
            requested_workspace_id: workspace.clone(),
            accepted_at: crate::turn_service::monotonic_accepted_at(),
            route: crate::dto::SessionTurnRouteDto::Execute,
            task_title: None,
            execution_goal: None,
            required_tool_chain: vec![],
            completion_contract: Default::default(),
            recovery_checkpoint: None,
            session_id: session.clone(),
            workspace_id: workspace,
            queue_id: input_id.into(),
            retry_count: 0,
        })?;
        Ok(())
    }
    pub(crate) fn invalidate_setup_continuation(
        &self,
        session: &SessionId,
        except_request: Option<&str>,
    ) -> Result<(), ApiError> {
        if let Some(mut record) = self.read_setup_record().filter(|record| {
            record.session_id == session.as_str()
                && record.continue_state.as_deref() == Some("waiting_channel")
        }) && let Some(input_id) = record
            .continuation_input_id
            .clone()
            .filter(|id| Some(id.as_str()) != except_request)
        {
            self.remove_regular_session_turn(session, &input_id)?;
            record.continue_state = Some("saved_not_continued".into());
            record.continuation_input_id = None;
            self.commit_setup_record(&mut record)?;
        }
        Ok(())
    }
    pub(crate) async fn settle_setup_continuation(
        &self,
        session: &SessionId,
        input_id: &str,
        accepted: bool,
    ) -> Result<(), ApiError> {
        let _serial = self.web_model.setup_lock.lock().await;
        if let Some(mut record) = self.read_setup_record().filter(|record| {
            record.session_id == session.as_str()
                && record.continuation_input_id.as_deref() == Some(input_id)
                && record.continue_state.as_deref() == Some("waiting_channel")
        }) {
            record.continuation_input_id = None;
            record.continue_state = Some(
                if accepted {
                    "continued"
                } else {
                    "saved_not_continued"
                }
                .into(),
            );
            self.commit_setup_record(&mut record)?;
        }
        Ok(())
    }
    pub(crate) fn wake_setup_continuation(&self) {
        let state = self.clone();
        tokio::spawn(async move {
            let Some(record) = state
                .read_setup_record()
                .filter(|record| record.continue_state.as_deref() == Some("waiting_channel"))
            else {
                return;
            };
            let session = SessionId::new(record.session_id);
            let turn = state.lock_session_turn_commit(&session).await;
            let serial = state.web_model.setup_lock.lock().await;
            let Some(mut record) = state.read_setup_record().filter(|record| {
                record.session_id == session.as_str()
                    && record.continue_state.as_deref() == Some("waiting_channel")
            }) else {
                return;
            };
            state.reconcile_setup_save(&mut record);
            if record.continue_state.as_deref() == Some("continued") {
                let _ = state.commit_setup_record(&mut record);
                return;
            }
            let input = record
                .continuation_input_id
                .as_ref()
                .and_then(|id| state.session_store.input_for_id(&session, id));
            let condition = input
                .and_then(|input| {
                    serde_json::from_value::<crate::state::QueuedRegularSessionTurn>(input.payload)
                        .ok()
                })
                .and_then(|turn| turn.readiness);
            let status = state.web_model.status();
            let expired = condition.as_ref().is_none_or(|condition| {
                UtcMillis::now().0 >= condition.deadline
                    || condition.runtime_epoch != state.runtime_epoch()
                    || condition.channel_generation != state.web_model.channel_generation()
            });
            let failed = !status.ready
                && !matches!(
                    status.code.as_str(),
                    "client_installing" | "runtime_not_ready"
                );
            if expired || failed || !state.web_model_setup_available() {
                if let Some(id) = record.continuation_input_id.as_ref() {
                    let _ = state.remove_regular_session_turn(&session, id);
                }
                record.continue_state = Some("saved_not_continued".into());
                record.run_state = Some("blocked".into());
                record.reason = if expired {
                    "channel_wait_expired"
                } else {
                    "channel_failed"
                }
                .into();
                record.detail = Some(
                    "凭据已保存，通道尚未就绪；自动继续已停止，请核对通道状态后明确继续".into(),
                );
                let _ = state.commit_setup_record(&mut record);
                return;
            }
            drop(serial);
            drop(turn);
            crate::turn_service::schedule_next_queued_regular_session_turn(state, session, None);
        });
    }
}
impl ApiState {
    pub(crate) async fn control_setup_request(
        &self,
        request: magi_app_server_protocol::generated::WebModelSetupControlParams,
    ) -> Result<Value, ApiError> {
        let session = SessionId::new(&request.session_id);
        let verify = |record: &WebModelSetupStatus| {
            record.session_id == request.session_id
                && record.setup_run_id.as_deref() == Some(request.setup_run_id.as_str())
                && (record.revision == Some(request.expected_revision)
                    || (record.revision == request.expected_revision.checked_add(1)
                        && record.saved_credential_version.is_some()
                        && matches!(
                            record.continue_state.as_deref(),
                            Some("waiting_channel" | "saved_not_continued")
                        )
                        && record
                            .save_request_binding
                            .as_ref()
                            .and_then(|binding| serde_json::from_str::<Value>(binding).ok())
                            .is_some_and(|binding| {
                                binding[1].as_i64() == Some(request.expected_revision)
                            })))
                && record.execution_generation == Some(request.execution_generation)
        };
        if let Some(scope) = self
            .web_model
            .setup_scope
            .lock()
            .expect("setup scope lock poisoned")
            .as_ref()
            && self.read_setup_record().as_ref().is_some_and(&verify)
        {
            scope.cancelled.cancel();
        }
        let _turn = self.lock_session_turn_commit(&session).await;
        let _serial = self.web_model.setup_lock.lock().await;
        let mut record = self.read_setup_record().filter(&verify).ok_or_else(|| {
            ApiError::Conflict("stale_setup: 配置执行已变化，旧控制请求不生效".into())
        })?;
        if matches!(record.run_state.as_deref(), Some("completed" | "revoked")) {
            return Err(ApiError::Conflict(
                "配置已结束，旧控制请求不能改写历史结果".into(),
            ));
        }
        if record.run_state.as_deref() == Some("cancelled") {
            return if request.action
                == magi_app_server_protocol::generated::WebModelSetupControlAction::Cancel
            {
                Ok(json!({"status":"succeeded","configuration":record}))
            } else {
                Err(ApiError::Conflict(
                    "配置已取消，不能通过旧请求暂停或恢复".into(),
                ))
            };
        }
        self.reconcile_setup_save(&mut record);
        self.close_setup_scope(&session, Some(request.execution_generation))
            .await?;
        self.invalidate_setup_continuation(&session, None)?;
        let cancel = request.action
            == magi_app_server_protocol::generated::WebModelSetupControlAction::Cancel;
        record.run_state = Some(if cancel { "cancelled" } else { "paused" }.into());
        record.reason = "user_stopped".into();
        record.handoff_id = None;
        record.continue_state = None;
        if cancel {
            record.stage = "cancelled".into();
            if let Some(slots) = self.web_model.bindings() {
                slots.release_maintenance(session.as_str());
            }
        }
        self.commit_setup_record(&mut record)?;
        Ok(json!({"status":"succeeded","configuration":record}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_event_bus::InMemoryEventBus;
    use std::sync::Arc;
    fn fixture() -> (ApiState, tempfile::TempDir, WebModelSetupStatus) {
        let dir = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            "setup-test",
            Arc::new(InMemoryEventBus::new(64)),
            Arc::new(magi_session_store::SessionStore::new()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        )
        .with_runtime_persistence(Arc::new(crate::RuntimeStatePersistence::new(
            dir.path(),
            dir.path().join("workspaces.json"),
            dir.path().join("knowledge.json"),
        )));
        let package =
            crate::bundled_plugins::bundled_plugin_package(magi_web_model::WEB_MODEL_PLUGIN_ID)
                .unwrap();
        {
            let mut manager = state.plugin_manager.lock().unwrap();
            manager
                .install(
                    &package,
                    magi_plugin_system::PluginSource::Builtin {
                        name: "test".into(),
                    },
                )
                .unwrap();
            manager
                .authorize(
                    magi_web_model::WEB_MODEL_PLUGIN_ID,
                    "application",
                    package.manifest().permissions.clone(),
                )
                .unwrap();
            manager
                .enable(magi_web_model::WEB_MODEL_PLUGIN_ID, "application")
                .unwrap();
            manager
                .activate(magi_web_model::WEB_MODEL_PLUGIN_ID, "application")
                .unwrap();
        }
        let mut record = WebModelSetupStatus {
            session_id: "session".into(),
            setup_run_id: Some("run".into()),
            revision: Some(3),
            execution_generation: Some(2),
            run_state: Some("waiting_user".into()),
            stage: "credentials".into(),
            handoff_id: Some("handoff".into()),
            ..Default::default()
        };
        state.commit_setup_record(&mut record).unwrap();
        (state, dir, record)
    }
    fn main_context(
        state: &ApiState,
        session: &str,
        task: &str,
    ) -> magi_tool_runtime::ToolExecutionContext {
        let session_id = SessionId::new(session);
        state
            .session_store
            .create_session(session_id.clone(), session)
            .unwrap();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            state.turn_coordinator(),
            &session_id,
            &format!("turn-{session}"),
            1,
            UtcMillis::now(),
            "running",
            "配置 GPT Web",
        );
        let task_id = magi_core::TaskId::new(task);
        state
            .task_store()
            .unwrap()
            .insert_task(magi_core::Task {
                task_id: task_id.clone(),
                mission_id: magi_core::MissionId::new("setup-mission"),
                root_task_id: task_id.clone(),
                parent_task_id: None,
                kind: magi_core::TaskKind::LocalAgent,
                title: "配置".into(),
                goal: "配置 GPT Web".into(),
                status: magi_core::TaskStatus::Running,
                dependency_ids: vec![],
                required_children: vec![],
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: Default::default(),
                recovery_checkpoint: None,
                knowledge_refs: vec![],
                workspace_scope: None,
                write_scope: None,
                input_refs: vec![],
                output_refs: vec![],
                evidence_refs: vec![],
                retry_count: 0,
                runtime_payload: Default::default(),
                created_at: UtcMillis::now(),
                updated_at: UtcMillis::now(),
            })
            .unwrap();
        magi_tool_runtime::ToolExecutionContext {
            caller_scope: magi_tool_runtime::ToolCallerScope::SessionMainline,
            session_id: Some(session_id),
            task_id: Some(task_id),
            browser_capability_snapshot: Some(magi_browser_authority::BrowserCapabilitySnapshot {
                revision: 1,
                in_app_browser_enabled: true,
                browser_use_enabled: true,
                devtools_enabled: true,
                lan_access_enabled: false,
                host_status: magi_browser_authority::BrowserHostStatus::Ready,
                host_protocol_compatible: true,
            }),
            ..Default::default()
        }
    }
    fn scope_fixture(state: &ApiState) -> crate::web_model_setup_tools::SetupScope {
        crate::web_model_setup_tools::SetupScope {
            run_id: "run".into(),
            session: SessionId::new("session"),
            task: magi_core::TaskId::new("task"),
            turn: "turn-session".into(),
            generation: 2,
            digest: state
                .plugin_manager
                .lock()
                .unwrap()
                .package(magi_web_model::WEB_MODEL_PLUGIN_ID)
                .unwrap()
                .digest()
                .into(),
            snapshot: Value::Null,
            catalog_cursor: 0,
            channel_generation: state.web_model.channel_generation(),
            cancelled: magi_plugin_system::RunCancellation::default(),
        }
    }
    #[tokio::test]
    async fn plugin_revocation_cancels_the_browser_scope_before_serial_cleanup() {
        use magi_web_model::WebModelPageDriver;
        let (state, dir, _) = fixture();
        let scope = scope_fixture(&state);
        let slots = Arc::new(magi_web_model::WebSlotTable::new());
        slots.reserve_maintenance("session").unwrap();
        state.web_model.set_bindings(slots.clone());
        *state.web_model.setup_scope.lock().unwrap() = Some(scope.clone());
        let _lease = magi_plugin_system::PluginRuntimeLease::acquire(
            state.plugin_manager.clone(),
            magi_web_model::WEB_MODEL_PLUGIN_ID,
            &scope.digest,
            "application",
            &scope.cancelled,
        )
        .unwrap();
        // 模拟已有页面动作持有串行锁；撤权不能等到清理取得该锁才生效。
        let serial = state.web_model.setup_lock.lock().await;
        state
            .plugin_manager
            .lock()
            .unwrap()
            .authorize(magi_web_model::WEB_MODEL_PLUGIN_ID, "application", vec![])
            .unwrap();
        assert!(scope.cancelled.is_cancelled());
        assert_eq!(
            state.read_setup_record().unwrap().run_state.as_deref(),
            Some("waiting_user")
        );
        let driver = crate::web_model_driver::HostWebModelPageDriver::new(
            Arc::new(std::sync::RwLock::new(None)),
            state.browser_authority.clone(),
            dir.path().join("uploads"),
        );
        let error = crate::web_model_driver::with_web_setup_turn(
            scope.turn.clone(),
            scope.cancelled.clone(),
            driver.setup_browser("browser_snapshot", &json!({})),
        )
        .await
        .unwrap_err();
        assert!(
            error.is_cancelled(),
            "必须在任何宿主读取和重试之前拒绝旧 scope"
        );
        drop(serial);
        state.cancel_web_model_setup_for(None).await;
        assert!(state.web_model.setup_scope.lock().unwrap().is_none());
        assert!(slots.maintenance_owner().is_none());
        assert_eq!(
            state.read_setup_record().unwrap().run_state.as_deref(),
            Some("revoked")
        );
    }
    #[tokio::test]
    async fn closing_a_scope_retires_clones_without_cancelling_a_new_generation() {
        let (state, _dir, _) = fixture();
        let old = scope_fixture(&state);
        *state.web_model.setup_scope.lock().unwrap() = Some(old.clone());
        state
            .close_setup_scope(&old.session, Some(1))
            .await
            .unwrap();
        assert!(!old.cancelled.is_cancelled());
        state
            .close_setup_scope(&SessionId::new("foreign"), Some(2))
            .await
            .unwrap();
        assert!(!old.cancelled.is_cancelled());
        state
            .close_setup_scope(&old.session, Some(2))
            .await
            .unwrap();
        assert!(old.cancelled.is_cancelled());
        let mut next = scope_fixture(&state);
        next.generation = 3;
        *state.web_model.setup_scope.lock().unwrap() = Some(next.clone());
        state
            .close_setup_scope(&old.session, Some(2))
            .await
            .unwrap();
        assert!(!next.cancelled.is_cancelled());
        assert_eq!(
            state
                .web_model
                .setup_scope
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .generation,
            3
        );
    }
    #[test]
    fn transfer_invalidates_old_controls_and_preserves_the_original_save_receipt() {
        let (state, _dir, mut record) = fixture();
        let state =
            state.with_task_store(Arc::new(magi_orchestrator::task_store::TaskStore::new()));
        let source = main_context(&state, "session", "task");
        let target = main_context(&state, "target", "target-task");
        let slots = Arc::new(magi_web_model::WebSlotTable::new());
        slots.reserve_maintenance("session").unwrap();
        state.web_model.set_bindings(slots.clone());
        record.save_request_id = Some("source-save".into());
        record.saved_credential_version = Some("credential-saved".into());
        state.commit_setup_record(&mut record).unwrap();
        let scope = scope_fixture(&state);
        *state.web_model.setup_scope.lock().unwrap() = Some(scope.clone());
        let invoke =
            |context, args: Value| state.execute_web_model_setup_tool(&args.to_string(), context);
        let (_, foreign_status) = invoke(
            &target,
            json!({"action":"transfer","setup_run_id":"run","expected_revision":3,"target_session_id":"session"}),
        );
        assert_eq!(foreign_status, magi_core::ExecutionResultStatus::Rejected);
        let (reply, status) = invoke(
            &source,
            json!({"action":"transfer","setup_run_id":"run","expected_revision":3,"target_session_id":"target"}),
        );
        assert_eq!(
            status,
            magi_core::ExecutionResultStatus::Succeeded,
            "{reply}"
        );
        let transferred = state.read_setup_record().unwrap();
        assert_eq!(transferred.session_id, "target");
        assert_eq!(transferred.run_state.as_deref(), Some("paused"));
        assert_eq!(transferred.execution_generation, Some(3));
        assert_eq!(slots.maintenance_owner().as_deref(), Some("target"));
        assert!(scope.cancelled.is_cancelled());
        assert!(transferred.handoff_id.is_none() && transferred.save_request_id.is_none());
        let receipt = state
            .setup_save_receipt(&SessionId::new("session"), "source-save")
            .unwrap();
        assert_eq!(receipt["saved"], true);
        assert!(
            state
                .setup_save_receipt(&SessionId::new("target"), "source-save")
                .is_err()
        );
        for args in [
            json!({"action":"pause","setup_run_id":"run","expected_revision":3}),
            json!({"action":"begin","setup_run_id":"run","expected_revision":3}),
        ] {
            assert_eq!(
                invoke(&source, args).1,
                magi_core::ExecutionResultStatus::Rejected
            );
        }
        assert_eq!(state.read_setup_record().unwrap(), transferred);
        assert_eq!(slots.maintenance_owner().as_deref(), Some("target"));
    }
    #[test]
    fn a_new_session_can_begin_after_another_owner_finishes_without_its_private_revision() {
        for (run_state, stage) in [
            (Some("cancelled"), "cancelled"),
            (Some("completed"), "complete"),
            (None, "complete"),
            (None, "cancelled"),
        ] {
            let (state, dir, mut record) = fixture();
            let state =
                state.with_task_store(Arc::new(magi_orchestrator::task_store::TaskStore::new()));
            let context = main_context(&state, "next-session", "next-task");
            record.run_state = run_state.map(str::to_string);
            record.stage = stage.into();
            record.save_request_id = Some("previous-save".into());
            record.saved_credential_version = Some("credential-saved".into());
            state.commit_setup_record(&mut record).unwrap();
            state
                .web_model
                .set_bindings(Arc::new(magi_web_model::WebSlotTable::new()));
            state.web_model.set_driver(Arc::new(
                crate::web_model_driver::HostWebModelPageDriver::new(
                    Arc::new(std::sync::RwLock::new(None)),
                    state.browser_authority.clone(),
                    dir.path().join("uploads"),
                ),
            ));
            let (inspection, status) = state
                .execute_web_model_setup_tool(&json!({"action":"inspect"}).to_string(), &context);
            assert_eq!(status, magi_core::ExecutionResultStatus::Succeeded);
            assert!(serde_json::from_str::<Value>(&inspection).unwrap()["configuration"].is_null());
            // 新拥有者从 inspect 得不到旧版本；不携带旧会话的私有版本即可建立新运行。
            let (_, status) = state
                .execute_web_model_setup_tool(&json!({"action":"begin"}).to_string(), &context);
            assert_eq!(
                status,
                magi_core::ExecutionResultStatus::Rejected,
                "本夹具没有 Desktop，应在物理页面恢复处停止"
            );
            let next = state.read_setup_record().unwrap();
            assert_eq!(next.session_id, "next-session");
            assert_ne!(next.setup_run_id, record.setup_run_id);
            assert_eq!(next.execution_generation, Some(3));
            assert_eq!(next.run_state.as_deref(), Some("blocked"));
            assert_eq!(next.reason, "surface_unavailable");
            assert!(state.web_model.setup_scope.lock().unwrap().is_none());
            assert_eq!(
                state
                    .setup_save_receipt(&SessionId::new("session"), "previous-save")
                    .unwrap()["saved"],
                true
            );
        }
    }
    #[test]
    fn an_unsettled_save_cannot_be_transferred_to_another_session() {
        let (state, _dir, mut record) = fixture();
        let state =
            state.with_task_store(Arc::new(magi_orchestrator::task_store::TaskStore::new()));
        let context = main_context(&state, "session", "task");
        main_context(&state, "target", "target-task");
        record.continue_state = Some("save_pending".into());
        record.pending_credential_version = Some("credential-unsettled".into());
        state.commit_setup_record(&mut record).unwrap();
        let (_, status) = state.execute_web_model_setup_tool(&json!({"action":"transfer","setup_run_id":"run","expected_revision":3,"target_session_id":"target"}).to_string(), &context);
        assert_eq!(status, magi_core::ExecutionResultStatus::Rejected);
        assert_eq!(state.read_setup_record().unwrap(), record);
    }
    #[test]
    fn plugin_manager_replacement_refreshes_live_skill_provider() {
        let (active, _dir, _) = fixture();
        let runtime = Arc::new(magi_skill_runtime::SkillRuntime::new(
            magi_skill_runtime::SkillRegistry::new(),
        ));
        let state = ApiState::new(
            "skill-wiring",
            Arc::new(InMemoryEventBus::new(8)),
            Arc::new(magi_session_store::SessionStore::new()),
            Arc::new(magi_workspace::WorkspaceStore::default()),
            Arc::new(magi_governance::GovernanceService::default()),
        )
        .with_skill_runtime(runtime.clone())
        .with_plugin_manager(active.plugin_manager.clone());
        assert!(
            runtime
                .registry()
                .get(crate::web_model_setup_tools::SKILL_ID)
                .is_some()
        );
        state
            .plugin_manager
            .lock()
            .unwrap()
            .deactivate(magi_web_model::WEB_MODEL_PLUGIN_ID, "application")
            .unwrap();
        assert!(
            runtime
                .registry()
                .get(crate::web_model_setup_tools::SKILL_ID)
                .is_none()
        );
    }
    #[test]
    fn save_receipt_uses_committed_private_version_not_global_key_presence() {
        let (state, _dir, mut record) = fixture();
        record.save_request_id = Some("request".into());
        record.pending_credential_version = Some("credential-test".into());
        record.continue_state = Some("save_pending".into());
        state.commit_setup_record(&mut record).unwrap();
        assert_eq!(
            state
                .setup_save_receipt(&SessionId::new("session"), "request")
                .unwrap()["saved"],
            false
        );
        let config = state
            .web_model
            .persist_credentials(
                &state,
                Default::default(),
                Some("sk-private-test"),
                Some("credential-other".into()),
            )
            .unwrap();
        assert_ne!(config.credential_version, record.pending_credential_version);
        assert_eq!(
            state
                .setup_save_receipt(&SessionId::new("session"), "request")
                .unwrap()["saved"],
            false
        );
        let config = state
            .web_model
            .persist_credentials(
                &state,
                config,
                Some("sk-private-test"),
                record.pending_credential_version.clone(),
            )
            .unwrap();
        let receipt = state
            .setup_save_receipt(&SessionId::new("session"), "request")
            .unwrap();
        assert_eq!(receipt["saved"], true);
        assert_eq!(receipt["status"], "saved_not_continued");
        assert!(!receipt.to_string().contains("sk-private-test"));
        assert_eq!(
            receipt["configuration"]["savedCredentialVersion"],
            json!(config.credential_version)
        );
        assert!(
            state
                .setup_save_receipt(&SessionId::new("foreign"), "request")
                .is_err()
        );
    }
    #[tokio::test]
    async fn replay_never_overwrites_secret_and_nonsecret_binding_changes_conflict() {
        let (state, _dir, mut record) = fixture();
        let request = || magi_app_server_protocol::generated::WebModelSetupCredentialsParams {
            session_id: "session".into(),
            setup_run_id: "run".into(),
            expected_revision: 3,
            handoff_id: "handoff".into(),
            request_id: "request".into(),
            tunnel_id: "tunnel_0123456789abcdef0123456789abcdef".into(),
            api_key: "sk-replayed-wrong".into(),
            continue_after_save: Some(true),
        };
        record.save_request_id = Some("request".into());
        record.save_request_binding =
            Some(json!(["run", 3, "handoff", request().tunnel_id, true]).to_string());
        record.saved_credential_version = Some("credential-original".into());
        record.continue_state = Some("saved_not_continued".into());
        state.commit_setup_record(&mut record).unwrap();
        let first = state
            .save_setup_credentials_request(request())
            .await
            .unwrap();
        let second = state
            .save_setup_credentials_request(request())
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(second["saved"], true);
        assert!(state.web_model.reveal_api_key().is_none());
        let mut changed = request();
        changed.handoff_id = "other".into();
        assert!(matches!(
            state.save_setup_credentials_request(changed).await,
            Err(ApiError::Conflict(_))
        ));
    }
    #[tokio::test]
    async fn stale_control_cannot_stop_new_generation_or_change_owner() {
        let (state, _dir, record) = fixture();
        let request =
            |generation| magi_app_server_protocol::generated::WebModelSetupControlParams {
                session_id: record.session_id.clone(),
                setup_run_id: "run".into(),
                expected_revision: 3,
                execution_generation: generation,
                action: magi_app_server_protocol::generated::WebModelSetupControlAction::Cancel,
            };
        assert!(state.control_setup_request(request(1)).await.is_err());
        assert_eq!(
            state.read_setup_record().unwrap().run_state.as_deref(),
            Some("waiting_user")
        );
        state.control_setup_request(request(2)).await.unwrap();
        assert_eq!(
            state.read_setup_record().unwrap().run_state.as_deref(),
            Some("cancelled")
        );
        state.control_setup_request(request(2)).await.unwrap();
    }
    #[test]
    fn restart_reconciles_save_but_does_not_restore_execution_or_handoff() {
        let (state, _dir, mut record) = fixture();
        record.pending_credential_version = Some("credential-test".into());
        record.continue_state = Some("save_pending".into());
        record.save_request_id = Some("request".into());
        state.commit_setup_record(&mut record).unwrap();
        state
            .web_model
            .persist_credentials(
                &state,
                Default::default(),
                Some("sk-private-test"),
                record.pending_credential_version.clone(),
            )
            .unwrap();
        state.restore_web_model_setup_reservation();
        let restored = state.read_setup_record().unwrap();
        assert_eq!(restored.run_state.as_deref(), Some("paused"));
        assert_eq!(restored.handoff_id, None);
        assert_eq!(restored.execution_generation, Some(3));
        assert_eq!(restored.revision, Some(4));
        assert_eq!(
            state
                .setup_save_receipt(&SessionId::new("session"), "request")
                .unwrap()["saved"],
            true
        );
        assert_eq!(
            state.read_setup_record().unwrap().revision,
            Some(4),
            "查询不再次递增配置版本"
        );
    }
    #[tokio::test]
    async fn cancellation_preserves_historical_completion() {
        let (state, _dir, mut record) = fixture();
        record.run_state = Some("completed".into());
        record.stage = "complete".into();
        state.commit_setup_record(&mut record).unwrap();
        state.cancel_web_model_setup_for(None).await;
        assert_eq!(state.read_setup_record().unwrap(), record);
        state.restore_web_model_setup_reservation();
        assert_eq!(state.read_setup_record().unwrap(), record);
        assert!(
            state
                .control_setup_request(
                    magi_app_server_protocol::generated::WebModelSetupControlParams {
                        session_id: record.session_id.clone(),
                        setup_run_id: "run".into(),
                        expected_revision: 3,
                        execution_generation: 2,
                        action:
                            magi_app_server_protocol::generated::WebModelSetupControlAction::Cancel,
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(state.read_setup_record().unwrap(), record);
    }
    #[tokio::test]
    async fn a_replaced_run_keeps_its_save_receipt_without_reopening_it() {
        let (state, _dir, mut original) = fixture();
        original.save_request_id = Some("saved-request".into());
        original.save_request_binding = Some(
            json!([
                "run",
                3,
                "handoff",
                "tunnel_0123456789abcdef0123456789abcdef",
                true
            ])
            .to_string(),
        );
        original.saved_credential_version = Some("credential-original".into());
        original.continue_state = Some("continued".into());
        state.commit_setup_record(&mut original).unwrap();
        let mut newer = WebModelSetupStatus {
            session_id: "new-owner".into(),
            setup_run_id: Some("new-run".into()),
            revision: Some(0),
            run_state: Some("running".into()),
            ..Default::default()
        };
        state.commit_setup_record(&mut newer).unwrap();
        let receipt = state
            .setup_save_receipt(&SessionId::new("session"), "saved-request")
            .unwrap();
        assert_eq!(receipt["saved"], true);
        assert_eq!(
            receipt["configuration"]["savedCredentialVersion"],
            "credential-original"
        );
        assert!(
            state
                .setup_save_receipt(&SessionId::new("new-owner"), "saved-request")
                .is_err()
        );
        state
            .plugin_manager
            .lock()
            .unwrap()
            .deactivate(magi_web_model::WEB_MODEL_PLUGIN_ID, "application")
            .unwrap();
        let replay = state
            .save_setup_credentials_request(
                magi_app_server_protocol::generated::WebModelSetupCredentialsParams {
                    session_id: "session".into(),
                    setup_run_id: "run".into(),
                    expected_revision: 3,
                    handoff_id: "handoff".into(),
                    request_id: "saved-request".into(),
                    tunnel_id: "tunnel_0123456789abcdef0123456789abcdef".into(),
                    api_key: "sk-must-not-be-written".into(),
                    continue_after_save: Some(true),
                },
            )
            .await
            .unwrap();
        assert_eq!(replay, receipt);
        assert_eq!(state.read_setup_record().unwrap(), newer);
        assert!(state.web_model.reveal_api_key().is_none());
    }
    #[test]
    fn a_failed_save_receipt_commit_is_reconciled_from_disk_once() {
        let (state, dir, mut record) = fixture();
        let settings_dir = dir.path().join("settings");
        let settings_path = settings_dir.join("settings.json");
        let state = state.with_settings_store(Arc::new(
            magi_settings_store::SettingsStore::with_persistence_path(settings_path.clone()),
        ));
        record.save_request_id = Some("request".into());
        record.pending_credential_version = Some("credential-test".into());
        record.continue_state = Some("save_pending".into());
        state.commit_setup_record(&mut record).unwrap();
        state
            .web_model
            .persist_credentials(
                &state,
                Default::default(),
                Some("sk-fault-injection-sentinel"),
                record.pending_credential_version.clone(),
            )
            .unwrap();

        // 凭据引用已提交后，让 setup 回执的原子写入失败。
        let backup = dir.path().join("settings-backup");
        std::fs::rename(&settings_dir, &backup).unwrap();
        std::fs::write(&settings_dir, "block directory").unwrap();
        assert!(
            state
                .setup_save_receipt(&SessionId::new("session"), "request")
                .is_err()
        );
        assert_eq!(
            state.read_setup_record().unwrap().continue_state.as_deref(),
            Some("save_pending")
        );
        std::fs::remove_file(&settings_dir).unwrap();
        std::fs::rename(&backup, &settings_dir).unwrap();

        let reloaded = Arc::new(magi_settings_store::SettingsStore::with_persistence_path(
            settings_path.clone(),
        ));
        reloaded.load_from_disk().unwrap();
        let restarted = state.with_settings_store(reloaded);
        restarted.restore_web_model_setup_reservation();
        let receipt = restarted
            .setup_save_receipt(&SessionId::new("session"), "request")
            .unwrap();
        assert_eq!(receipt["saved"], true);
        assert_eq!(receipt["status"], "saved_not_continued");
        assert_eq!(receipt["configuration"]["revision"], 4);
        assert_eq!(
            receipt,
            restarted
                .setup_save_receipt(&SessionId::new("session"), "request")
                .unwrap()
        );
        assert!(
            !std::fs::read_to_string(settings_path)
                .unwrap()
                .contains("sk-fault-injection-sentinel")
        );
    }
    #[tokio::test]
    async fn continuation_receipt_settles_once_and_ignores_other_inputs() {
        for accepted in [true, false] {
            let (state, _dir, mut record) = fixture();
            record.continue_state = Some("waiting_channel".into());
            record.continuation_input_id = Some("continue-input".into());
            state.commit_setup_record(&mut record).unwrap();
            state
                .settle_setup_continuation(&SessionId::new("session"), "other", accepted)
                .await
                .unwrap();
            assert_eq!(
                state.read_setup_record().unwrap().continue_state.as_deref(),
                Some("waiting_channel")
            );
            state
                .settle_setup_continuation(&SessionId::new("session"), "continue-input", accepted)
                .await
                .unwrap();
            let settled = state.read_setup_record().unwrap();
            assert_eq!(
                settled.continue_state.as_deref(),
                Some(if accepted {
                    "continued"
                } else {
                    "saved_not_continued"
                })
            );
            assert!(settled.continuation_input_id.is_none());
            state
                .settle_setup_continuation(&SessionId::new("session"), "continue-input", !accepted)
                .await
                .unwrap();
            assert_eq!(state.read_setup_record().unwrap(), settled);
        }
    }
}
