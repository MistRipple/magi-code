//! 公开扩展服务的原生实现。调用归属由 daemon 租约注入；业务事实仍沿原有服务提交。
use super::*;
use crate::model_step::StepModelClient;
use magi_core::ThreadId;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelInput {
    prompt: String,
    role_id: Option<String>,
    credential_ref: Option<String>,
    max_output_tokens: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotInput {
    session_id: Option<String>,
    after_turn_seq: Option<u64>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialInput {
    role_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotifyInput {
    message: String,
    title: Option<String>,
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("InvalidParams: {error}"))
}

struct Owner {
    session: SessionId,
    turn: String,
    task: Option<magi_core::Task>,
    thread: ThreadId,
    worker_id: Option<WorkerId>,
}

impl LlmTaskDispatcher {
    fn extension_owner(&self, context: &Value) -> Result<Owner, String> {
        let session = context
            .pointer("/owner/sessionId")
            .and_then(Value::as_str)
            .ok_or("缺少 daemon 调用归属")?;
        let session = SessionId::new(session);
        let task = context
            .pointer("/owner/taskId")
            .and_then(Value::as_str)
            .map(|id| {
                self.pipeline
                    .task_store.as_ref()
                    .get_task(&TaskId::new(id))
                    .ok_or("调用任务不存在")
            })
            .transpose()?;
        let turn = if let Some(task) = &task {
            self.session_store
                .agent_turn_for_task(&session, &task.task_id)
                .map_err(|e| e.to_string())?
                .map(|turn| turn.turn_id)
                .or_else(|| {
                    self.session_store
                        .runtime_sidecar(&session)
                        .and_then(|s| s.current_turn)
                        .filter(|t| {
                            t.items
                                .iter()
                                .any(|i| i.task_id.as_ref() == Some(&task.task_id))
                        })
                        .map(|t| t.turn_id)
                })
        } else {
            context
                .pointer("/owner/turnId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }
        .ok_or("调用没有活动 Turn")?;
        if !self.session_store.turn_execution_is_active(&session, &turn) {
            return Err("调用 Turn 已停止".into());
        }
        if task.as_ref().is_some_and(|task| {
            !matches!(
                task.status,
                magi_core::TaskStatus::Running | magi_core::TaskStatus::Pending
            )
        }) {
            return Err("调用任务已停止".into());
        }
        // 扩展模型分支与原生会话分支隔离缓存谱系，不混入主模型的历史请求。
        let thread = task
            .as_ref()
            .and_then(|task| {
                self.session_store
                    .thread_registry_snapshot(&session)
                    .into_iter()
                    .find(|thread| thread.handled_task_ids.contains(&task.task_id))
                    .map(|thread| thread.thread_id)
            })
            .or_else(|| {
                self.session_store
                    .orchestrator_thread_for_session(&session)
                    .map(|thread| thread.thread_id)
            })
            .ok_or("会话缺少原生 Thread")?;
        let worker_id = task
            .as_ref()
            .and_then(|task| {
                self.pipeline
                    .task_store.as_ref()
                    .get_active_lease(&task.task_id)
            })
            .map(|lease| lease.worker_id);
        Ok(Owner {
            session,
            turn,
            task,
            thread,
            worker_id,
        })
    }

    /// 只接受 daemon 注入的 _context，调用方不能直接使用此内部装配入口。
    pub fn extension_service(
        &self,
        plugin: &str,
        method: &str,
        mut params: Value,
        cancelled: Arc<AtomicBool>,
        progress: Option<magi_extension_host::ServiceProgress>,
    ) -> Result<Value, String> {
        if method == "session.message" {
            return self
                .plugin_session_message_admitter
                .as_ref()
                .ok_or("会话消息服务未装配")?(plugin, params, cancelled);
        }
        let context = params
            .as_object_mut()
            .ok_or("服务参数必须是对象")?
            .remove("_context")
            .unwrap_or(Value::Null);
        if cancelled.load(Ordering::Acquire) {
            return Err("Cancelled: 调用已取消".into());
        }
        if matches!(method, "model.selection" | "model.cancel")
            || method == "model.invoke" && context.pointer("/owner/sessionId").is_none()
        {
            if context.get("activity").and_then(Value::as_str) != Some("route") {
                return Err("ReentryDenied: 插件模型业务需要活动页面路由".into());
            }
            return self.plugin_model_service(plugin, method, params, cancelled, progress);
        }
        if method == "session.directory" {
            use magi_app_server_protocol::generated::{
                PluginSessionDirectory, PluginSessionDirectoryEntry, PluginSessionDirectoryRequest,
            };
            let input: PluginSessionDirectoryRequest = parse(params)?;
            let limit = input.limit.unwrap_or(100);
            if !(1..=200).contains(&limit)
                || [
                    input.workspace_id.as_deref(),
                    input.after_session_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|value| value.trim().is_empty() || value.len() > 256)
            {
                return Err("InvalidParams: 会话目录筛选或分页参数无效".into());
            }
            // 元数据直接读取原生 SessionStore，不加载消息、任务、路径或配置；
            // 与用户目录一样排除外部工具会话，按稳定身份分页避免改名改变游标。
            let mut records = match input.workspace_id.as_deref() {
                Some(workspace) => self.session_store.sessions_for_workspace(workspace),
                None => self.session_store.sessions(),
            };
            records.retain(|record| {
                record.kind.is_user()
                    && input
                        .after_session_id
                        .as_deref()
                        .is_none_or(|after| record.session_id.as_str() > after)
            });
            records.sort_by(|a, b| a.session_id.as_str().cmp(b.session_id.as_str()));
            let has_more = records.len() > limit;
            let sessions: Vec<_> = records
                .into_iter()
                .take(limit)
                .map(|record| PluginSessionDirectoryEntry {
                    session_id: record.session_id.to_string(),
                    workspace_id: record.workspace_id,
                    title: record.title,
                    created_at: record.created_at.0,
                    updated_at: record.updated_at.0,
                })
                .collect();
            let next_after_session_id = if has_more {
                sessions.last().map(|entry| entry.session_id.clone())
            } else {
                None
            };
            return serde_json::to_value(PluginSessionDirectory {
                sessions,
                has_more,
                next_after_session_id,
            })
            .map_err(|error| error.to_string());
        }
        if method == "session.snapshot" {
            let input: SnapshotInput = parse(params)?;
            let session = input
                .session_id
                .or_else(|| {
                    context
                        .pointer("/owner/sessionId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .ok_or("缺少 sessionId")?;
            let session = SessionId::new(session);
            let record = self
                .session_store
                .session_record(&session)
                .ok_or("会话不存在")?;
            let turns = self.session_store.canonical_turns_for_session(&session);
            let mut matching = turns
                .into_iter()
                .filter(|t| t.turn_seq > input.after_turn_seq.unwrap_or(0));
            let limit = input.limit.unwrap_or(5).clamp(1, 10);
            let mut page = Vec::new();
            for turn in matching.by_ref().take(limit) {
                let items = turn.items.iter().rev().take(20).map(|i| json!({"id":i.item_id,"kind":i.kind,"status":i.status,"text":i.content.as_ref().map(|s| s.chars().take(1024).collect::<String>())})).collect::<Vec<_>>();
                page.push(json!({"turnId":turn.turn_id,"turnSeq":turn.turn_seq,"status":turn.status,"items":items,"itemsTruncated":turn.items.len()>20}));
            }
            let cursor = page.last().and_then(|v| v["turnSeq"].as_u64());
            return Ok(
                json!({"session":record,"turns":page,"nextAfterTurnSeq":cursor,"hasMore":matching.next().is_some(),"projection":"bounded-text-v1"}),
            );
        }
        let owner = self.extension_owner(&context)?;
        if method == "credential.ref" {
            // 引用当前配置的模型连接；该引用只由 model.invoke 消费，不解密或返回 apiKey。
            let input: CredentialInput = parse(params)?;
            // 与随后的 model.invoke 使用同一个原生解析入口。主模型由会话
            // 选择，不能在这里只检查不包含 model 的全局连接设置。
            self.ensure_extension_model_source(
                owner.task.as_ref(),
                input.role_id.as_deref(),
                &owner.session,
            )?;
            self.resolve_model_client_for_task(
                self.settings_store.as_ref(),
                owner.task.as_ref(),
                input.role_id.as_deref(),
                Some(&owner.session),
            )?;
            return Ok(
                json!({"ref":input.role_id.map(|role| format!("model-role:{role}")).unwrap_or_else(|| "model-default".into()),"kind":"model-binding"}),
            );
        }
        let is_active = || {
            !cancelled.load(Ordering::Acquire)
                && self
                    .session_store
                    .turn_execution_is_active(&owner.session, &owner.turn)
        };
        if !is_active() {
            return Err("Cancelled: 调用已结束".into());
        }
        match method {
            "model.invoke" => {
                let input: ModelInput = parse(params)?;
                if input.prompt.trim().is_empty() || input.prompt.len() > 1024 * 1024 {
                    return Err("prompt 必须非空且不超过 1 MiB".into());
                }
                let ref_role = match input.credential_ref.as_deref() {
                    None | Some("model-default") => None,
                    Some(reference) => Some(
                        reference
                            .strip_prefix("model-role:")
                            .filter(|v| !v.is_empty())
                            .ok_or("凭据引用类型不受支持")?
                            .to_string(),
                    ),
                };
                if input.role_id.is_some() && ref_role.is_some() && input.role_id != ref_role {
                    return Err("roleId 与凭据引用不一致".into());
                }
                let role = input.role_id.or(ref_role);
                self.ensure_extension_model_source(
                    owner.task.as_ref(),
                    role.as_deref(),
                    &owner.session,
                )?;
                let client = self.resolve_model_client_for_task(
                    self.settings_store.as_ref(),
                    owner.task.as_ref(),
                    role.as_deref(),
                    Some(&owner.session),
                )?;
                let mut binding = crate::usage_recording::session_turn_model_usage_binding(false);
                if let Some(role) = role
                    .as_deref()
                    .or_else(|| owner.task.as_ref().and_then(|t| task_role_id(Some(t))))
                {
                    binding = crate::usage_recording::extension_model_usage_binding(
                        self.settings_store.as_ref(),
                        role,
                    )?;
                }
                if let Some(task) = &owner.task {
                    binding = binding.with_task_model(task, self.settings_store.as_ref())?;
                }
                let call = EventId::unique(format!("extension-model-{plugin}")).to_string();
                let client = StepModelClient::new(
                    client.as_ref(),
                    &self.session_store,
                    &owner.session,
                    Some(&owner.turn),
                    owner.task.as_ref().map(|t| &t.task_id),
                    &owner.thread,
                    &call,
                    0,
                )
                .with_model_binding(self.settings_store.as_ref(), &binding);
                let request = ModelInvocationRequest {
                    cache_affinity_key: None,
                    prompt_cache_key: None,
                    provider: BUSINESS_MODEL_PROVIDER.into(),
                    prompt: input.prompt,
                    messages: None,
                    tools: None,
                    tool_choice: None,
                };
                let limit = input.max_output_tokens.unwrap_or(4096);
                if !(1..=32768).contains(&limit) {
                    return Err("maxOutputTokens 范围为 1..32768".into());
                }
                let response = client
                    .invoke_with_cancellation_and_output_limit(request, limit, &|| !is_active())
                    .map_err(|e| e.to_string())?;
                crate::usage_recording::publish_model_usage_record_for_turn(
                    &self.event_bus,
                    &self.session_store,
                    self.settings_store.as_ref(),
                    Some(&owner.turn),
                    crate::usage_recording::ModelUsageRecordInput {
                        session_id: &owner.session,
                        workspace_id: &self
                            .session_store
                            .session_record(&owner.session)
                            .and_then(|s| s.workspace_id)
                            .map(WorkspaceId::new),
                        binding: &binding,
                        call_id: call,
                        usage: response.usage.as_ref(),
                        context_breakdown: None,
                        status: magi_usage_authority::UsageCallStatus::Success,
                        assignment_id: None,
                        error_code: None,
                    },
                );
                if !is_active() {
                    return Err("Cancelled: 模型结果未交付".into());
                }
                serde_json::to_value(response).map_err(|e| e.to_string())
            }
            "notify.user" => {
                let input: NotifyInput = parse(params)?;
                if input.message.trim().is_empty()
                    || input.message.len() > 4096
                    || input.title.as_ref().is_some_and(|t| t.len() > 256)
                {
                    return Err("通知为空或超过长度上限".into());
                }
                let id = EventId::unique("extension-notification").to_string();
                self.session_store
                    .append_incident_record(magi_session_store::NotificationRecord {
                        notification_id: id.clone(),
                        scope: magi_session_store::NotificationScope::Session,
                        workspace_id: None,
                        session_id: Some(owner.session.clone()),
                        kind: "incident".into(),
                        level: Some("info".into()),
                        title: input.title,
                        message: input.message,
                        detail: None,
                        error_code: None,
                        failure_stage: None,
                        task_id: owner.task.as_ref().map(|t| t.task_id.to_string()),
                        request_id: None,
                        source: Some(format!("plugin:{plugin}")),
                        created_at: UtcMillis::now(),
                        handled: false,
                        action_required: false,
                        count_unread: true,
                        fingerprint: id.clone(),
                        occurrence_count: 1,
                        resolved: false,
                    })
                    .map_err(|e| e.to_string())?;
                self.session_state_persist_callback
                    .as_ref()
                    .ok_or("通知持久化服务未配置")?("extension-notification")?;
                self.event_bus.publish(
                    EventEnvelope::domain(
                        EventId::unique("notification"),
                        "notification.published",
                        json!({"session_id":owner.session,"notification_id":id}),
                    )
                    .with_context(EventContext {
                        session_id: Some(owner.session),
                        ..Default::default()
                    }),
                );
                Ok(json!({"notificationId":id}))
            }
            "ask.user" => self.extension_ask(plugin, params, &owner, &is_active),
            "task.spawn" | "tool.call" => {
                self.extension_tool(plugin, method, params, &owner, &is_active)
            }
            _ => Err(format!("MethodNotFound: {method}")),
        }
    }
}

impl LlmTaskDispatcher {
    fn extension_tool_item(
        &self,
        plugin: &str,
        owner: &Owner,
        tool: &str,
        input: &Value,
    ) -> magi_session_store::ActiveExecutionTurnItem {
        let call = EventId::unique("extension-action").to_string();
        let mut item = crate::session_writeback::session_turn_item(
            "tool_call_started",
            "running",
            Some(tool.into()),
            None,
            Some(call.clone()),
            owner.thread.clone(),
        );
        item.task_id = owner.task.as_ref().map(|t| t.task_id.clone());
        item.worker_id = owner.worker_id.clone();
        item.tool_call_id = Some(call);
        item.tool_name = Some(tool.into());
        item.tool_status = Some("running".into());
        item.tool_arguments = Some(input.to_string());
        item.metadata.insert("pluginId".into(), json!(plugin));
        item
    }

    fn extension_commit_item(
        &self,
        owner: &Owner,
        item: magi_session_store::ActiveExecutionTurnItem,
    ) -> Result<(), String> {
        let store = self.pipeline.task_store.as_ref();
        let sink = crate::session_writeback::CanonicalTurnEventSink::new(
            &self.session_store,
            &self.event_bus,
            Some(store),
        );
        let published = sink
            .upsert_item(&owner.session, Some(&owner.turn), item)?
            .ok_or("Turn 已结束，扩展动作未提交")?;
        let workspace = self
            .session_store
            .session_record(&owner.session)
            .and_then(|s| s.workspace_id)
            .map(WorkspaceId::new);
        sink.publish_item(&owner.session, &workspace, &published);
        Ok(())
    }

    fn extension_ask(
        &self,
        plugin: &str,
        params: Value,
        owner: &Owner,
        active: &(dyn Fn() -> bool + Sync),
    ) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            questions: Vec<Value>,
        }
        let input: Input = parse(params.clone())?;
        let questions = crate::user_question::parse_questions(
            &json!({"questions":input.questions}).to_string(),
        )?;
        let mut item = self.extension_tool_item(plugin, owner, "ask_user_question", &params);
        let question = crate::PendingUserQuestion {
            question_id: EventId::unique("extension-question").to_string(),
            session_id: owner.session.clone(),
            task_id: owner.task.as_ref().map(|t| t.task_id.clone()),
            turn_id: owner.turn.clone(),
            tool_call_id: item.tool_call_id.clone().unwrap(),
            questions: questions.clone(),
            requested_at: UtcMillis::now(),
            can_skip: true,
        };
        item.tool_result = Some(json!({"status":"awaiting_user_input","question_id":question.question_id,"questions":questions}).to_string());
        self.extension_commit_item(owner, item.clone())?;
        let registry = self.conversation_registry.user_questions();
        let waiter = registry.request_persisted(&self.session_store, question.clone())?;
        // 取消路径同样发布 resolved，界面不会留下孤立等待状态。
        let publish = |event: &str, outcome: Option<&str>| {
            self.event_bus.publish(EventEnvelope::domain(EventId::unique(event), event, json!({"session_id":owner.session,"task_id":question.task_id,"turn_id":owner.turn,"tool_call_id":question.tool_call_id,"question_id":question.question_id,"outcome":outcome})).with_context(EventContext {session_id:Some(owner.session.clone()), task_id:question.task_id.clone(), ..Default::default()}));
        };
        publish("user.question.requested", None);
        let human_wait = self
            .conversation_registry
            .human_wait()
            .begin(&owner.session, true);
        let response = loop {
            if !active() {
                registry.cancel(&question.question_id);
                publish("user.question.resolved", Some("cancelled"));
                return Err("Cancelled: 提问已取消".into());
            }
            match waiter
                .response_rx
                .recv_timeout(std::time::Duration::from_millis(100))
            {
                Ok(response) => break response,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    registry.cancel(&question.question_id);
                    return Err("Cancelled: 提问等待结束".into());
                }
            }
        };
        if !human_wait.finish(active) {
            return Err("Cancelled: 回答未交付".into());
        }
        // 答复已由 SessionStore 的唯一交互提交入口持久化，不用旧 item 覆盖其 metadata。
        publish("user.question.resolved", Some("answered"));
        Ok(json!({"questionId":question.question_id,"response":response}))
    }
}

impl LlmTaskDispatcher {
    fn extension_tool(
        &self,
        plugin: &str,
        method: &str,
        params: Value,
        owner: &Owner,
        active: &(dyn Fn() -> bool + Sync),
    ) -> Result<Value, String> {
        use magi_bridge_client::{ChatToolCall, ChatToolFunction};
        use magi_core::ExecutionResultStatus;
        let task = owner
            .task
            .as_ref()
            .ok_or("该动作需要活动 Task；插件命令没有子任务归属")?;
        let store = self.pipeline.task_store.as_ref();
        let lease = store
            .get_active_lease(&task.task_id)
            .ok_or("Task 执行租约已失效")?;
        let registry = self.tool_registry.as_ref().ok_or("工具服务未装配")?;
        let (name, input) = if method == "task.spawn" {
            ("agent_spawn".to_string(), params)
        } else {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Tool {
                tool: String,
                input: Value,
            }
            let input: Tool = parse(params)?;
            // 扩展工具可直接使用 Node OS API。这里仅开放原生工具，防止跨扩展递归调用环。
            if magi_tool_runtime::BuiltinToolName::from_name(&input.tool).is_none() {
                return Err(
                    "tool.call 当前只接受公开原生工具；不允许调用扩展自身或其它扩展".into(),
                );
            }
            (input.tool, input.input)
        };
        let definitions = constrain_task_tool_definitions(
            public_builtin_tool_definitions(registry),
            task.policy_snapshot.as_ref(),
        )
        .into_iter()
        .filter(|definition| {
            BuiltinToolName::from_name(&definition.function.name).is_some_and(|tool| {
                task_can_see_builtin_tool(Some(task), Some(&self.agent_role_registry), tool)
            })
        })
        .collect::<Vec<_>>();
        let definition = definitions
            .iter()
            .find(|d| d.function.name == name)
            .ok_or("工具未在当前 Task 策略下公开")?;
        crate::tool_call_validation::validate_json_schema_value(
            &input,
            &definition.function.parameters,
        )?;
        let workspace = self
            .session_store
            .session_record(&owner.session)
            .and_then(|s| s.workspace_id)
            .map(WorkspaceId::new);
        let root = self.resolve_task_source_root(task, &owner.session, &workspace);
        let prepared = self.prepare_task_workspace_execution(
            task,
            &lease.lease_id,
            &owner.session,
            &workspace,
            root.as_ref(),
            false,
            Some(&lease.worker_id),
        )?;
        let mut item = self.extension_tool_item(plugin, owner, &name, &input);
        let call = ChatToolCall {
            id: item.tool_call_id.clone().unwrap(),
            kind: "function".into(),
            function: ChatToolFunction {
                name: name.clone(),
                arguments: input.to_string(),
            },
        };
        let hooked = self.extension_hooks.as_deref().map(|hooks| {
            crate::extension_hooks::prepare_tool_calls(
                hooks,
                &crate::extension_hooks::ToolHookScope {
                    session_id: owner.session.as_str(),
                    task_id: task.task_id.as_str(),
                    workspace_id: workspace.as_ref().map(|w| w.as_str()),
                    worker_id: Some(lease.worker_id.as_str()),
                    round: 0,
                },
                std::slice::from_ref(&call),
                &definitions,
            )
        });
        let original = [call.clone()];
        let calls = hooked
            .as_ref()
            .map(|h| h.execute.as_slice())
            .unwrap_or(&original);
        // canonical 记录最终入参；后续权限、审批、目录与并发分类均基于这些内容。
        if let Some(final_call) = calls.first() {
            item.tool_arguments = Some(final_call.function.arguments.clone());
        }
        let mut ledger = crate::tool_execution_ledger::ToolExecutionLedger::from_canonical_task(
            &self.session_store,
            store,
            &owner.session,
            task,
            &owner.thread,
            Some(registry),
        )?
        .ledger;
        self.extension_commit_item(owner, item.clone())?;
        let progress_item = Mutex::new(item.clone());
        let progress = |value: magi_tool_runtime::ToolExecutionProgress| {
            let mut item = progress_item.lock().map_err(|_| "工具进度锁已损坏")?;
            item.tool_result = Some(value.payload);
            self.extension_commit_item(owner, item.clone())
        };
        let plan = magi_plan::PlanStore::from_store(&self.session_store, owner.session.clone());
        let safety = safety_gate_from_settings(self.settings_store.as_ref());
        let allowed = || {
            active()
                && store
                    .get_active_lease(&task.task_id)
                    .is_some_and(|current| current.lease_id == lease.lease_id)
        };
        let results = crate::tool_batch::execute_task_tool_call_batch(
            &self.event_bus,
            Some(registry),
            &self.agent_role_registry,
            self.skill_runtime.as_deref(),
            self.skill_dispatch_runtime.as_deref(),
            None,
            store,
            &self.session_store,
            &self.execution_registry,
            &self.conversation_registry,
            safety.as_ref(),
            &plan,
            None,
            &prepared.task,
            &owner.session,
            &workspace,
            prepared.root.as_ref(),
            Some(&lease.worker_id),
            None,
            calls,
            &mut ledger,
            Some(&progress),
            Some(prepared.resources),
            None,
            &allowed,
        );
        let mut results = hooked
            .map(|hooked| hooked.merge(results.clone()))
            .unwrap_or(results);
        let (output, status) = results.pop().ok_or("工具没有结算回执")?;
        item.tool_result = Some(output.clone());
        item.kind = "tool_call_result".into();
        item.tool_status = Some(status.wire_label().into());
        item.status = match status {
            ExecutionResultStatus::Succeeded => "completed",
            ExecutionResultStatus::Cancelled => "cancelled",
            ExecutionResultStatus::Indeterminate => "indeterminate",
            _ => "failed",
        }
        .into();
        self.extension_commit_item(owner, item)?;
        Ok(
            json!({"status":status.wire_label(),"output":serde_json::from_str::<Value>(&output).unwrap_or(Value::String(output))}),
        )
    }
}
