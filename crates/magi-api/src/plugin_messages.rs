//! 插件消息复用原生 Input 收件箱和 Turn 引导；不保存另一份消息或执行状态。
use crate::{ApiState, dto::SessionTurnRequestDto, state::QueuedRegularSessionTurn};
use magi_app_server_protocol::generated::{
    PluginSessionMessageDelivery as Delivery, PluginSessionMessageOrigin,
    PluginSessionMessageRequest,
};
use magi_core::{SessionId, TaskId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// 在 dispatcher/Runner 装配前创建，与目录准备端口使用相同的无环装配顺序。
pub fn plugin_session_message_admitter(
    state: &ApiState,
) -> Arc<magi_conversation_runtime::plugin_command::PluginSessionMessageAdmitter> {
    let state = state.clone();
    let runtime = tokio::runtime::Handle::current();
    Arc::new(move |plugin, params, cancelled| {
        runtime.block_on(admit(&state, plugin, params, cancelled))
    })
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn receipt(input: magi_session_store::SessionInput) -> Value {
    use magi_app_server_protocol::generated::{
        PluginSessionMessageReceipt, PluginSessionMessageState,
    };
    use magi_session_store::SessionInputState;
    serde_json::to_value(PluginSessionMessageReceipt {
        input_id: input.input_id,
        request_id: input.request_id,
        state: match input.state {
            SessionInputState::Pending => PluginSessionMessageState::Pending,
            SessionInputState::Attached => PluginSessionMessageState::Attached,
            SessionInputState::Consumed => PluginSessionMessageState::Consumed,
            SessionInputState::Cancelled => PluginSessionMessageState::Cancelled,
        },
        turn_id: input.attached_turn_id,
    })
    .expect("receipt serializes")
}

async fn admit(
    state: &ApiState,
    plugin: &str,
    mut params: Value,
    cancelled: Arc<AtomicBool>,
) -> Result<Value, String> {
    let context = params
        .as_object_mut()
        .ok_or("消息参数必须是对象")?
        .remove("_context")
        .ok_or("缺少 daemon 调用归属")?;
    let input: PluginSessionMessageRequest =
        serde_json::from_value(params).map_err(|error| format!("消息参数无效：{error}"))?;
    if input.text.trim().is_empty()
        || input.text.len() > 200_000
        || input.message_id.trim().is_empty()
        || input.message_id.len() > 128
        || input.source_kind.trim().is_empty()
        || input.source_kind.len() > 128
        || input.source_kind.chars().any(char::is_control)
        || matches!(input.source_kind.as_str(), "user" | "system" | "assistant")
    {
        return Err("消息正文、身份或来源无效；插件不能冒充用户或系统".into());
    }
    let session = SessionId::new(
        context
            .pointer("/owner/sessionId")
            .and_then(Value::as_str)
            .ok_or("缺少当前会话")?,
    );
    let task_id = TaskId::new(
        context
            .pointer("/owner/taskId")
            .and_then(Value::as_str)
            .ok_or("消息需要真实 Task 归属")?,
    );
    let store = state.task_store().ok_or("TaskStore 未装配")?;
    let source_turn = state
        .task_execution_registry()
        .turn_id(&task_id)
        .ok_or("调用任务缺少 Turn")?;
    let task = store.get_task(&task_id).ok_or("调用任务不存在")?;
    if !matches!(state.task_execution_registry().get(&task_id),
        Some(magi_conversation_runtime::task_execution_registry::TaskExecutionPlan::Dispatch { session_id, .. }) if session_id == session)
    {
        return Err("Task 不属于当前会话".into());
    }
    if task.parent_task_id.is_some() {
        return Err("子任务不能向主会话发送消息".into());
    }
    let lease = store
        .get_active_lease(&task_id)
        .ok_or("消息的 Task 租约已失效")?;
    let active = || {
        !cancelled.load(Ordering::Acquire)
            && store.execution_lease_is_current(&task_id, &lease.lease_id)
            && state.task_execution_registry().turn_id(&task_id).as_deref()
                == Some(source_turn.as_str())
            && state
                .session_store
                .turn_execution_is_active(&session, &source_turn)
    };
    let _guard = state.lock_session_turn_commit(&session).await;
    if !active() {
        return Err("调用已停止，消息未接纳".into());
    }
    if context
        .pointer("/owner/turnId")
        .and_then(Value::as_str)
        .is_some_and(|turn| turn != source_turn)
    {
        return Err("调用 Turn 与当前任务不一致".into());
    }
    if !context.get("job").is_none_or(Value::is_null) && input.delivery != Delivery::Context {
        return Err("计划作业不能绕过签发预算另起模型轮次".into());
    }
    let target = if input.delivery == Delivery::Context {
        "context"
    } else {
        "wake"
    };
    let name = {
        let manager = state
            .plugin_manager
            .lock()
            .map_err(|_| "插件管理器不可用")?;
        if !manager.is_active(plugin).map_err(|e| e.to_string())?
            || !manager
                .permission_allowed(
                    plugin,
                    magi_plugin_system::PluginPermissionKind::SessionWrite,
                    target,
                )
                .map_err(|e| e.to_string())?
        {
            return Err(format!("缺少 sessionWrite:[{target}] 授权或插件已停用"));
        }
        manager
            .package(plugin)
            .map_err(|e| e.to_string())?
            .manifest()
            .name
            .clone()
    };
    let origin = PluginSessionMessageOrigin {
        plugin_id: plugin.into(),
        plugin_name: name,
        message_id: input.message_id.clone(),
        source_kind: input.source_kind.clone(),
        source_turn_id: source_turn.clone(),
        source_task_id: task_id.to_string(),
        delivery: input.delivery.clone(),
    };
    let identity = format!(
        "plugin-message-{}",
        hash(&serde_json::to_vec(&(plugin, &session, &input.message_id)).unwrap())
    );
    let binding = task
        .executor_binding
        .as_ref()
        .and_then(|binding| binding.project_directories.clone());
    let workspace = binding.as_ref().map(|binding| binding.workspace_id.clone());
    let mut request: SessionTurnRequestDto = serde_json::from_value(json!({
        "scope":if workspace.is_some(){"workspace"}else{"personal"},"sessionId":session,
        "workspaceId":workspace,"text":input.text,"requestId":identity,"userMessageId":format!("user-{identity}"),
        "accessProfile":task.policy_snapshot.as_ref().map(|policy|policy.access_profile).unwrap_or_default(),
    })).map_err(|error|error.to_string())?;
    request.project_directories = binding.clone();
    request.source_folder_id = binding
        .as_ref()
        .map(|binding| binding.default_source_folder_id.clone());
    let fingerprint = format!(
        "sha256:{}",
        hash(&serde_json::to_vec(&(&request, &origin)).map_err(|e| e.to_string())?)
    );
    if let Some(existing) = state.session_store.input_for_request(&identity) {
        if existing.session_id != session || existing.request_fingerprint != fingerprint {
            return Err("同一消息身份的内容或来源已经改变".into());
        }
        return Ok(receipt(existing));
    }
    let pending = state.queued_regular_session_turns(&session);
    if pending
        .iter()
        .filter(|turn| {
            turn.plugin_message
                .as_ref()
                .is_some_and(|origin| origin.plugin_id == plugin)
        })
        .count()
        >= 32
    {
        return Err("插件待处理消息达到上限，请等待处理或取消已有消息".into());
    }
    let scope = crate::scope_binding::require_session_request_scope(
        state,
        Some(session.as_str()),
        request.scope,
        request.requested_workspace_id().as_deref(),
        None,
    )
    .map_err(|e| e.message().to_string())?
    .scope;
    crate::task_dispatch::validate_or_migrate_directory_binding(
        state,
        workspace.as_ref(),
        binding.clone(),
    )
    .map_err(|e| e.message().to_string())?;
    let command = state
        .session_store
        .canonical_turn_for_session_turn_id(&session, &source_turn)
        .is_some_and(|turn| {
            turn.items
                .iter()
                .any(|item| item.metadata.contains_key("pluginCommandId"))
        });
    if !command && input.delivery != Delivery::Followup {
        request.steer_current_turn = true;
        request.expected_turn_id = Some(source_turn.clone());
        // 与用户的运行中补充共用提交锁、canonical 事务和模型输入边界。
        match crate::turn_service::submit_steer_current_turn_after_turn_commit(
            state,
            &request,
            &scope,
            crate::turn_service::monotonic_accepted_at(),
            &fingerprint,
            Some(&origin),
        )
        .await
        {
            Ok(_) => {
                return state
                    .session_store
                    .input_for_request(&identity)
                    .map(receipt)
                    .ok_or("消息接纳后缺少 Input 回执".into());
            }
            Err(error) if matches!(error, crate::ApiError::TurnConflict { .. }) => {}
            Err(error) => return Err(error.message().into()),
        }
        request.steer_current_turn = false;
        request.expected_turn_id = None;
    }
    if !active() {
        return Err("调用已停止，消息未接纳".into());
    }
    let accepted_at = crate::turn_service::monotonic_accepted_at();
    state
        .enqueue_regular_session_turn(QueuedRegularSessionTurn {
            plugin_message: Some(origin),
            request,
            project_directories: binding,
            request_fingerprint: Some(fingerprint),
            requested_workspace_id: workspace.clone(),
            accepted_at,
            route: crate::dto::SessionTurnRouteDto::Execute,
            task_title: Some(input.source_kind),
            execution_goal: Some(input.text),
            required_tool_chain: vec![],
            completion_contract: Default::default(),
            recovery_checkpoint: None,
            session_id: session,
            workspace_id: workspace,
            queue_id: identity.clone(),
            retry_count: 0,
        })
        .map_err(|error| error.message().to_string())?;
    state
        .session_store
        .input_for_request(&identity)
        .map(receipt)
        .ok_or("消息提交后缺少 Input 回执".into())
}
