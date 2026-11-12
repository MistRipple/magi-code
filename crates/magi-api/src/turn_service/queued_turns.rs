use super::*;

pub(super) struct EnqueueSessionTurnInput<'a> {
    pub(super) state: &'a ApiState,
    pub(super) request: SessionTurnRequestDto,
    pub(super) request_fingerprint: String,
    pub(super) requested_workspace_id: Option<WorkspaceId>,
    pub(super) accepted_at: UtcMillis,
    pub(super) decision: SessionTurnIntentDecision,
    pub(super) session_id: SessionId,
    pub(super) workspace_id: Option<WorkspaceId>,
}

pub(super) fn enqueue_session_turn_response(
    input: EnqueueSessionTurnInput<'_>,
) -> Result<SessionTurnResponseDto, ApiError> {
    let EnqueueSessionTurnInput {
        state,
        mut request,
        request_fingerprint,
        requested_workspace_id,
        accepted_at,
        decision,
        session_id,
        workspace_id,
    } = input;
    let queue_id = format!("queued-session-turn-{}-{}", session_id, accepted_at.0);
    let (request_id, user_message_item_id) = normalize_session_turn_identity(
        request.request_id.take(),
        request.user_message_id.take(),
        &queue_id,
    );
    request.request_id = Some(request_id.clone());
    request.user_message_id = Some(user_message_item_id.clone());
    let queued_request_id = Some(request_id);
    let queued_user_message_id = Some(user_message_item_id.clone());
    let queue_position = state.enqueue_regular_session_turn(QueuedRegularSessionTurn {
        plugin_message: None,
        project_directories: request.project_directories.clone(),
        request,
        request_fingerprint: Some(request_fingerprint),
        requested_workspace_id,
        accepted_at,
        route: decision.route,
        task_title: decision.task_title.clone(),
        execution_goal: decision.execution_goal.clone(),
        required_tool_chain: decision.required_tool_chain.clone(),
        completion_contract: decision.completion_contract.clone(),
        recovery_checkpoint: decision.recovery_checkpoint.clone(),
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        queue_id: queue_id.clone(),
        retry_count: 0,
    })?;
    let (event_id, event_sequence) = publish_regular_session_turn_queued_event(
        state,
        &session_id,
        workspace_id.as_ref(),
        accepted_at,
        decision.route,
        &queue_id,
        queue_position,
        queued_request_id.clone(),
        queued_user_message_id,
    );
    Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id,
        entry_id: queue_id.clone(),
        event_id: event_id.clone(),
        accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: state.event_bus.snapshot().next_sequence,
        created_session: false,
        route: decision.route,
        root_task_id: None,
        action_task_id: None,
        execution_chain_ref: None,
        user_message_item_id: Some(user_message_item_id),
    })
    .with_queued(queue_id, queue_position)
    .with_request_identity(queued_request_id, event_sequence))
}

pub(crate) fn schedule_next_queued_regular_session_turn(
    state: ApiState,
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
) {
    let work = async move {
        if !state
            .session_store
            .session(&session_id)
            .is_some_and(|session| session.status == SessionLifecycleStatus::Active)
        {
            return;
        }
        if let Err(error) = state.settle_revoked_goal_agent_runs(&session_id).await {
            tracing::error!(%session_id, ?error, "Goal 代理尚未结算，暂不派发下一轮输入或自动续跑");
            return;
        }
        // 用户明确提交的下一轮输入先于自动续跑；Goal 在队列为空后继续。
        if drain_next_queued_regular_session_turn(
            state.clone(),
            session_id.clone(),
            workspace_id.clone(),
        )
        .await
            == QueuedRegularSessionTurnDrainOutcome::Empty
        {
            schedule_goal_continuation_turn_if_idle(state, session_id, workspace_id).await;
        }
    };
    crate::state::spawn_session_turn_work("magi-session-turn-queue", work);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueuedRegularSessionTurnDrainOutcome {
    Empty,
    Deferred,
    Started,
    RetainedAfterFailure,
    /// 队首消息不可重试或重试次数耗尽，已移出队列并通知用户，后续消息继续执行。
    DroppedAfterFailure,
}

/// 可重试的排队提交失败最多重试的次数；超过后按失败移出队列，不再阻塞后续消息。
pub(super) const MAX_QUEUED_SUBMISSION_RETRIES: u8 = 5;

/// active chain 是当前执行指针；终态收口会清空它，原 Turn 的 Task 来源仍在
/// canonical item 中。只接受属于该 Turn 的真实主线 root，不能用当前新任务
/// 或任意子代理代替已接纳停止意图的目标。
pub(super) fn session_turn_root_task_id(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: &str,
) -> Option<magi_core::TaskId> {
    state
        .session_store
        .active_execution_chain(session_id)
        .filter(|chain| chain.current_turn_id.as_deref() == Some(turn_id))
        .map(|chain| chain.root_task_id)
        .or_else(|| {
            let turn = state
                .session_store
                .canonical_turn_for_session_turn_id(session_id, turn_id)?;
            if !turn.status.is_terminal() || turn.completed_at.is_none() {
                return None;
            }
            let store = state.task_store()?;
            turn.items.iter().find_map(|item| {
                let task_id = item.worker.as_ref()?.task_id.as_ref()?;
                let task = store.get_task(task_id)?;
                (task.parent_task_id.is_none() && task.task_id == task.root_task_id)
                    .then_some(task.task_id)
            })
        })
}

/// 调用方持有同一 Session 提交锁。删除替代输入也必须先结算旧执行，
/// 否则删掉屏障后普通队列可能在旧进程仍运行时抢跑。
pub(crate) async fn settle_queued_input_interruption_after_turn_commit(
    state: &ApiState,
    session_id: &SessionId,
    queued: &QueuedRegularSessionTurn,
) -> Result<(), ApiError> {
    fn retain_interruption_error(
        session_store: &magi_session_store::SessionStore,
        mut input: magi_session_store::SessionInput,
        message: String,
    ) {
        // 结算可能被多个调度唤醒同时观察到。错误本身已经是 durable 屏障时，
        // 重复观察不能继续推进 revision，否则客户端刚拿到的 revision 会被
        // 无意义地淘汰，后续 retry/remove 也会互相制造冲突。
        if input
            .interruption
            .as_ref()
            .and_then(|interruption| interruption.error.as_deref())
            == Some(message.as_str())
        {
            return;
        }
        let revision = input.revision;
        let Some(next_revision) = input.revision.checked_add(1) else {
            tracing::error!(input_id = %input.input_id, "输入 revision 溢出，保留原结算屏障");
            return;
        };
        input.revision = next_revision;
        if let Some(interruption) = input.interruption.as_mut() {
            interruption.error = Some(message);
        }
        if let Err(error) = session_store.commit_input(input, Some(revision)) {
            tracing::error!(?error, "记录停止并改做结算失败原因失败");
        }
    }

    if let Some(mut input) = state
        .session_store
        .input_for_id(session_id, &queued.queue_id)
        && let Some(interruption) = input
            .interruption
            .as_ref()
            .filter(|value| value.settled_at.is_none())
    {
        let source_root = session_turn_root_task_id(state, session_id, &interruption.turn_id);
        if source_root != interruption.root_task_id {
            tracing::error!(%session_id, queue_id = %queued.queue_id, "停止并改做的原执行来源已变化，输入保留待核对");
            retain_interruption_error(
                &state.session_store,
                input,
                "原任务来源已变化，无法确认资源结算；输入保留，请核对原任务".into(),
            );
            return Err(ApiError::Conflict(
                "原执行尚未完成资源结算，替代输入保留待处理".into(),
            ));
        }
        // 接纳已持久化。完成边界/重启不会丢失取消意图，也不能把目标改投到新的 Turn。
        if let Err(error) = interrupt_session_turn_after_turn_commit(
            state,
            session_id,
            queued.request.requested_workspace_id().as_deref(),
            queued.request.requested_workspace_path().as_deref(),
            Some(&interruption.turn_id),
        )
        .await
        {
            tracing::error!(%session_id, queue_id = %queued.queue_id, ?error, "停止并改做尚未结算，保留原输入");
            retain_interruption_error(&state.session_store, input, error.message().to_string());
            return Err(ApiError::Conflict(
                "原执行尚未完成资源结算，替代输入保留待处理".into(),
            ));
        }
        let revision = input.revision;
        input.revision = input.revision.checked_add(1).ok_or_else(|| {
            ApiError::Conflict("输入 revision 溢出，原执行结算屏障保留待核对".into())
        })?;
        input
            .interruption
            .as_mut()
            .expect("interruption checked")
            .settled_at = Some(UtcMillis::now().max(input.accepted_at));
        input
            .interruption
            .as_mut()
            .expect("interruption checked")
            .error = None;
        if let Err(error) = state.session_store.commit_input(input, Some(revision)) {
            tracing::error!(%session_id, queue_id = %queued.queue_id, ?error, "旧执行已停止但结算回执提交失败，保留输入等待核对");
            return Err(ApiError::Conflict(
                "原执行尚未完成资源结算，替代输入保留待处理".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn drain_next_queued_regular_session_turn(
    state: ApiState,
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
) -> QueuedRegularSessionTurnDrainOutcome {
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    if state.queued_regular_session_turn_count(&session_id) == 0 {
        return QueuedRegularSessionTurnDrainOutcome::Empty;
    }
    let Some(queued) = state.peek_next_regular_session_turn(&session_id) else {
        return QueuedRegularSessionTurnDrainOutcome::Empty;
    };
    if settle_queued_input_interruption_after_turn_commit(&state, &session_id, &queued)
        .await
        .is_err()
    {
        return QueuedRegularSessionTurnDrainOutcome::RetainedAfterFailure;
    }
    if state
        .session_store
        .ensure_current_turn_acceptance_available(&session_id)
        .is_err()
    {
        return QueuedRegularSessionTurnDrainOutcome::Deferred;
    }
    state.release_session_git_execution_lease(&session_id);
    let failed_event_session_id = queued.session_id.clone();
    let failed_event_workspace_id = queued.workspace_id.clone();
    let failed_event_route = queued.route;
    let failed_event_accepted_at = queued.accepted_at;
    let failed_event_queue_id = queued.queue_id.clone();
    let failed_event_request_id = queued.request.request_id();
    let failed_event_user_message_id = queued.request.user_message_id();
    let decision = SessionTurnIntentDecision {
        route: queued.route,
        task_title: queued.task_title.clone(),
        execution_goal: queued.execution_goal.clone(),
        required_tool_chain: queued.required_tool_chain.clone(),
        completion_contract: queued.completion_contract.clone(),
        recovery_checkpoint: queued.recovery_checkpoint.clone(),
    };
    let queued_request_fingerprint = queued
        .request_fingerprint
        .clone()
        .or_else(|| queued.request.request_fingerprint().ok());
    let queued_route = queued.route;
    let queued_goal_mode = queued.request.goal_mode;
    // 替代输入可越过更早的 FIFO 项；随后绑定的旧项不能倒排到已执行的新 Turn 前。
    let admission_at = if state
        .session_store
        .canonical_turns_for_session(&session_id)
        .iter()
        .any(|turn| turn.turn_seq >= queued.accepted_at.0)
    {
        monotonic_accepted_at()
    } else {
        queued.accepted_at
    };
    let plugin_message = queued.plugin_message;
    let mut queued_request = queued.request;

    queued_request.project_directories = queued.project_directories;
    // 出队时重新验证请求声明的 session scope。排队期间请求结构可能来自持久化
    // 恢复，不能只相信入队时解析出的 workspace_id；否则一个被篡改或失效的
    // workspace binding 会绕过同一条 Turn 提交入口的权限边界。
    let queued_scope = require_session_request_scope(
        &state,
        Some(session_id.as_str()),
        queued_request.scope,
        queued_request.requested_workspace_id().as_deref(),
        queued_request.requested_workspace_path().as_deref(),
    );
    let queued_scope = queued_scope.and_then(|scope| {
        queued_request.project_directories =
            crate::task_dispatch::validate_or_migrate_directory_binding(
                &state,
                scope.workspace_id().as_ref(),
                queued_request.project_directories.take(),
            )?;
        crate::task_dispatch::validate_project_context_references(
            queued_request.project_directories.as_ref(),
            &queued_request.context_references(),
        )?;
        Ok(scope)
    });
    let submit_result = match queued_scope {
        Err(error) => Err(error),
        Ok(scope) => match queued_request.parsed_images() {
            Ok(images) => match queued_route {
                SessionTurnRouteDto::Chat if !queued_goal_mode => {
                    match queued_request_fingerprint {
                        Some(request_fingerprint) => submit_conversation_session_turn(
                            state.clone(),
                            queued_request,
                            images,
                            scope.workspace_id(),
                            admission_at,
                            request_fingerprint,
                        )
                        .await
                        .map(|_| ()),
                        None => Err(ApiError::InvalidInput(
                            "排队消息缺少请求指纹，无法提交".to_string(),
                        )),
                    }
                }
                SessionTurnRouteDto::Chat | SessionTurnRouteDto::Execute => {
                    match queued_request_fingerprint {
                        Some(creation_request_fingerprint) => {
                            submit_mainline_session_turn(MainlineSessionTurnInput {
                                state: state.clone(),
                                request: queued_request,
                                images,
                                workspace_id: scope.workspace_id(),
                                accepted_at: admission_at,
                                decision,
                                creation_request_fingerprint,
                                job: None,
                                plugin_message,
                            })
                            .await
                            .map(|_| ())
                        }
                        None => Err(ApiError::InvalidInput(
                            "排队消息缺少请求指纹，无法提交".into(),
                        )),
                    }
                }
                SessionTurnRouteDto::Continue | SessionTurnRouteDto::Steer => Err(
                    ApiError::internal_assembly("执行排队 session turn", "不支持的排队 route"),
                ),
            },
            Err(error) => Err(ApiError::InvalidInput(format!(
                "排队消息图片输入无效: {error}"
            ))),
        },
    };
    match submit_result {
        Ok(_) => QueuedRegularSessionTurnDrainOutcome::Started,
        Err(error) => {
            tracing::error!(
                session_id = %failed_event_session_id,
                workspace_id = ?failed_event_workspace_id,
                queue_id = %failed_event_queue_id,
                error = ?error,
                "queued regular session turn failed before acceptance"
            );
            let retry_count = match state
                .record_regular_session_turn_retry(&failed_event_session_id, &failed_event_queue_id)
            {
                Ok(retry_count) => retry_count,
                Err(persist_error) => {
                    tracing::error!(
                        session_id = %failed_event_session_id,
                        queue_id = %failed_event_queue_id,
                        ?persist_error,
                        "排队消息仍保留，但重试计数持久化失败"
                    );
                    return QueuedRegularSessionTurnDrainOutcome::RetainedAfterFailure;
                }
            };
            if error.queued_submission_is_retryable() && retry_count < MAX_QUEUED_SUBMISSION_RETRIES
            {
                let retry_delay = std::time::Duration::from_secs(
                    u64::from(retry_count).saturating_mul(2).min(60),
                );
                let retry_state = state.clone();
                let retry_session_id = session_id.clone();
                let retry_workspace_id = workspace_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(retry_delay).await;
                    schedule_next_queued_regular_session_turn(
                        retry_state,
                        retry_session_id,
                        retry_workspace_id,
                    );
                });
            } else {
                publish_regular_session_turn_queue_failed_event(
                    &state,
                    &failed_event_session_id,
                    failed_event_workspace_id.clone(),
                    failed_event_accepted_at,
                    failed_event_route,
                    &failed_event_queue_id,
                    failed_event_request_id,
                    failed_event_user_message_id,
                    retry_count,
                    error.message(),
                );
                // 不可重试或重试耗尽的消息不能留在队首阻塞后续消息：移出队列（失败事件
                // 已告知用户，可重新发送），然后继续执行下一条。
                if let Err(remove_error) = state
                    .remove_regular_session_turn(&failed_event_session_id, &failed_event_queue_id)
                {
                    tracing::error!(
                        session_id = %failed_event_session_id,
                        queue_id = %failed_event_queue_id,
                        ?remove_error,
                        "移除失败的排队消息失败"
                    );
                    return QueuedRegularSessionTurnDrainOutcome::RetainedAfterFailure;
                }

                schedule_next_queued_regular_session_turn(
                    state.clone(),
                    failed_event_session_id,
                    failed_event_workspace_id,
                );
                return QueuedRegularSessionTurnDrainOutcome::DroppedAfterFailure;
            }
            QueuedRegularSessionTurnDrainOutcome::RetainedAfterFailure
        }
    }
}

pub(crate) fn canonical_turn_matches_queued_regular_turn(
    turn: &CanonicalTurn,
    queued: &QueuedRegularSessionTurn,
) -> bool {
    let Some(request_id) = queued.request.request_id() else {
        return false;
    };
    let Some(user_message_id) = queued.request.user_message_id() else {
        return false;
    };
    turn.items.iter().any(|item| {
        item.kind == CanonicalTurnItemKind::UserMessage
            && item
                .metadata
                .get("requestId")
                .and_then(|value| value.as_str())
                == Some(request_id.as_str())
            && item
                .metadata
                .get("userMessageId")
                .and_then(|value| value.as_str())
                == Some(user_message_id.as_str())
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_regular_session_turn_queued_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&magi_core::WorkspaceId>,
    accepted_at: UtcMillis,
    route: SessionTurnRouteDto,
    queue_id: &str,
    queue_position: usize,
    request_id: Option<String>,
    user_message_id: Option<String>,
) -> (EventId, u64) {
    // 由排队身份推导：turn/start 重放排队请求时按同一规则找回这条事件的序号。
    let event_id = EventId::new(format!("event-session-turn-queued-{queue_id}"));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.queued",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.map(ToString::to_string),
            "route": route,
            "queue_id": queue_id,
            "queue_position": queue_position,
            "request_id": request_id,
            "user_message_id": user_message_id,
            "turn_id": serde_json::Value::Null,
            "accepted_at": accepted_at.0,
            "queued_at": accepted_at,
        }),
    )
    .with_context(EventContext {
        workspace_id: workspace_id.cloned(),
        session_id: Some(session_id.clone()),
        ..EventContext::default()
    });
    let event_sequence = state.event_bus.publish(event);
    (event_id, event_sequence)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_regular_session_turn_queue_failed_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<WorkspaceId>,
    accepted_at: UtcMillis,
    route: SessionTurnRouteDto,
    queue_id: &str,
    request_id: Option<String>,
    user_message_id: Option<String>,
    retry_count: u8,
    direct_error: &str,
) {
    let event_id = EventId::unique("event-session-turn-queue-failed");
    let event = EventEnvelope::domain(
        event_id,
        "session.turn.queue_failed",
        json!({
            "session_id": session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "route": route,
            "queue_id": queue_id,
            "request_id": request_id,
            "user_message_id": user_message_id,
            // 队列消息尚未进入 canonical Turn，不能用 queueId 冒充 turnId。
            "turn_id": serde_json::Value::Null,
            "accepted_at": accepted_at.0,
            "retry_count": retry_count,
            "error": "queued_session_turn_failed",
            "error_code": "queued_session_turn_failed",
            "failure_detail": public_runtime_excerpt(direct_error, 4096),
            "public_message": "排队消息暂未执行，内容已保留；服务恢复后发送新消息即可继续。",
        }),
    )
    .with_context(EventContext {
        workspace_id,
        session_id: Some(session_id.clone()),
        ..EventContext::default()
    });
    state.event_bus.publish(event);
}
