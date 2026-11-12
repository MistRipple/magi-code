use super::*;

pub(super) fn publish_session_turn_continue_event(
    state: &ApiState,
    accepted: &SessionContinueAccepted,
) -> Result<EventId, ApiError> {
    let event_id = EventId::unique("event-session-turn-continue");
    let workspace_id = session_workspace_for_event(state, &accepted.session_id);
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.continue.executed",
        json!({
            "session_id": accepted.session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "mission_id": accepted.mission_id.to_string(),
            "root_task_id": accepted.root_task_id.to_string(),
            "execution_chain_ref": accepted.execution_chain_ref.clone(),
            "resumed_branch_count": accepted.resumed_branch_count,
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
        mission_id: Some(accepted.mission_id.clone()),
        task_id: Some(accepted.root_task_id.clone()),
        ..EventContext::default()
    });
    state.event_bus.publish(event);
    Ok(event_id)
}

pub(crate) fn turn_status_is_interruptible(status: &str) -> bool {
    !matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed"
            | "complete"
            | "succeeded"
            | "success"
            | "failed"
            | "error"
            | "interrupted"
            | "cancelled"
            | "canceled"
            | "superseded"
    )
}

pub(super) fn map_turn_replacement_error(
    state: &ApiState,
    session_id: &SessionId,
    error: DomainError,
) -> ApiError {
    match error {
        DomainError::InvalidState { .. } => ApiError::turn_conflict(
            "turn_not_latest",
            state
                .session_store
                .runtime_sidecar(session_id)
                .and_then(|sidecar| sidecar.current_turn.map(|turn| turn.turn_id)),
            "最近一条消息已发生变化，请基于最新会话重新编辑",
        ),
        other => ApiError::internal_assembly("替换最近一条消息失败", other),
    }
}

pub(crate) struct ContinueUserMessageInput<'a> {
    pub(crate) state: &'a ApiState,
    pub(crate) accepted: &'a SessionContinueAccepted,
    pub(crate) prompt_text: Option<&'a str>,
    pub(crate) control_only: bool,
    pub(crate) continued_at: UtcMillis,
    pub(crate) request_id: Option<String>,
    pub(crate) user_message_id: Option<String>,
    pub(crate) placeholder_message_id: Option<String>,
    pub(crate) request_fingerprint: Option<String>,
    pub(crate) attempt_id: Option<String>,
    pub(crate) orchestrator_thread_id: magi_core::ThreadId,
    pub(crate) resumed_thread_ids: Vec<magi_core::ThreadId>,
}

pub(crate) fn write_continue_user_message(
    input: ContinueUserMessageInput<'_>,
) -> Result<(String, Option<String>), ApiError> {
    let ContinueUserMessageInput {
        state,
        accepted,
        prompt_text,
        control_only,
        continued_at,
        request_id,
        user_message_id,
        placeholder_message_id,
        request_fingerprint,
        attempt_id,
        orchestrator_thread_id,
        resumed_thread_ids,
    } = input;
    let entry_id = format!("timeline-{}-{}", accepted.session_id, continued_at.0);
    let (user_message_item_id, items, user_message) = if let Some(prompt_text) = prompt_text {
        let (user_message_item_id, user_message_item) =
            build_user_message_turn_item(UserMessageTurnItemInput {
                accepted_at: continued_at,
                message: prompt_text,
                entry_id: &entry_id,
                request_id: request_id.clone(),
                user_message_id,
                placeholder_message_id,
                metadata: {
                    let mut metadata = std::collections::HashMap::from([(
                        "route".to_string(),
                        serde_json::Value::String("continue".to_string()),
                    )]);
                    if control_only {
                        metadata.insert("renderable".to_string(), serde_json::Value::Bool(false));
                    }
                    if let Some(request_fingerprint) = request_fingerprint {
                        metadata.insert(
                            "requestFingerprint".to_string(),
                            serde_json::Value::String(request_fingerprint),
                        );
                    }
                    metadata.insert(
                        "executionProfile".to_string(),
                        serde_json::Value::String("task".to_string()),
                    );
                    if let Some(request_id) = request_id.as_ref() {
                        metadata.insert(
                            "requestId".to_string(),
                            serde_json::Value::String(request_id.clone()),
                        );
                    }
                    if let Some(attempt_id) = attempt_id {
                        metadata.insert(
                            "attemptId".to_string(),
                            serde_json::Value::String(attempt_id),
                        );
                    }
                    metadata
                },
                task_id: Some(accepted.action_task_id.clone()),
                source_thread_id: orchestrator_thread_id.clone(),
            });
        (
            Some(user_message_item_id),
            vec![user_message_item],
            Some(prompt_text.to_string()),
        )
    } else {
        // 自动 Continue 没有新的用户文本，但 request/attempt 身份仍必须进入
        // canonical Turn，供重启后的 Coordinator replay 和终态收口使用。
        let mut metadata = std::collections::HashMap::from([
            (
                "route".to_string(),
                serde_json::Value::String("continue".to_string()),
            ),
            (
                "executionProfile".to_string(),
                serde_json::Value::String("task".to_string()),
            ),
        ]);
        if let Some(request_fingerprint) = request_fingerprint.clone() {
            metadata.insert(
                "requestFingerprint".to_string(),
                serde_json::Value::String(request_fingerprint),
            );
        }
        if let Some(request_id) = request_id.clone() {
            metadata.insert(
                "requestId".to_string(),
                serde_json::Value::String(request_id),
            );
        }
        if let Some(attempt_id) = attempt_id.clone() {
            metadata.insert(
                "attemptId".to_string(),
                serde_json::Value::String(attempt_id),
            );
        }
        let mut phase_item = magi_conversation_runtime::session_writeback::session_turn_item(
            "assistant_phase",
            "completed",
            None,
            None,
            Some(format!("turn-item-continue-phase-{}", accepted.turn_id)),
            orchestrator_thread_id.clone(),
        );
        phase_item.task_id = Some(accepted.action_task_id.clone());
        phase_item.metadata = metadata;
        phase_item
            .metadata
            .insert("renderable".to_string(), serde_json::Value::Bool(false));
        (None, vec![phase_item], None)
    };
    let timeline_kind = if user_message.is_some() {
        TimelineEntryKind::UserMessage
    } else {
        TimelineEntryKind::NotificationPublished
    };
    let timeline_message = user_message.as_deref().unwrap_or("继续执行中断的任务");
    let mut turn = ActiveExecutionTurn {
        turn_id: accepted.turn_id.clone(),
        turn_seq: continued_at.0,
        accepted_at: continued_at,
        status: "running".to_string(),
        completed_at: None,
        user_message: user_message.clone(),
        items,
    };
    if let Some(source) = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .filter(|source| source.status == "interrupted")
    {
        for item in &mut turn.items {
            item.metadata.insert(
                "recoverySourceTurnId".into(),
                serde_json::json!(source.turn_id),
            );
        }
    }
    for item in &mut turn.items {
        item.metadata.insert(
            "continuedThreadIds".into(),
            serde_json::json!(resumed_thread_ids),
        );
    }
    turn.normalize();
    state
        .turn_event_sink()
        .accept_turn_with_timeline_entry(
            accepted.session_id.clone(),
            TimelineEntryInput::new(
                entry_id.clone(),
                timeline_kind,
                timeline_message,
                continued_at,
            ),
            turn,
        )
        .map_err(|error| ApiError::internal_assembly("写入 continue 用户消息失败", error))?;
    state.persist_session_state_checkpoint("session_continue_user_message")?;
    if let Some(user_message) = user_message {
        publish_session_user_message_event(
            state,
            &accepted.session_id,
            session_workspace_for_event(state, &accepted.session_id),
            &user_message,
        );
    }
    Ok((entry_id, user_message_item_id))
}

pub(crate) fn finalize_terminal_root_current_turn(
    state: &ApiState,
    session_id: &SessionId,
    current_turn: Option<&ActiveExecutionTurn>,
) -> bool {
    if !current_turn.is_some_and(|turn| turn_status_is_interruptible(&turn.status)) {
        return false;
    }
    let Some(task_store) = state.task_store() else {
        return false;
    };
    let Some(chain) = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.active_execution_chain)
    else {
        return false;
    };
    let Some(root_task) = task_store.get_task(&chain.root_task_id) else {
        return false;
    };
    let runner_status = match root_task.status {
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "error",
        TaskStatus::Killed => "killed",
        _ => return false,
    };
    match crate::task_turn_finalize::finalize_background_session_task_turn_if_root_terminal_for_turn(
        state,
        session_id,
        &chain.root_task_id,
        runner_status,
        current_turn.map(|turn| turn.turn_id.as_str()),
    ) {
        Ok(finalized) => finalized,
        Err(error) => {
            tracing::error!(
                %session_id,
                root_task_id = %chain.root_task_id,
                %error,
                "终态根任务收口会话 Turn 失败"
            );
            false
        }
    }
}

pub(crate) fn finalize_continue_session(state: ApiState, accepted: SessionContinueAccepted) {
    let Some(_task_store) = state.task_store() else {
        return;
    };

    // 所有 tier 的 dispatch 驱动统一交给后台 RunnerManager：runner 已在
    // `continue_execution_chain` 中重新启动；终态由 TaskCompletionNotifier 收口。

    if let Err(error) =
        state.persist_session_projection_for_sessions(std::slice::from_ref(&accepted.session_id))
    {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            action_task_id = %accepted.action_task_id,
            ?error,
            "session continue finalize persist failed"
        );
    }
}

pub(super) fn current_turn_streaming_timeline_entry_ids(
    state: &ApiState,
    session_id: &SessionId,
) -> Vec<String> {
    state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .map(|turn| {
            turn.items
                .into_iter()
                .filter(|item| item.kind == "assistant_stream")
                .filter_map(|item| {
                    item.timeline_entry_id
                        .filter(|entry_id| !entry_id.trim().is_empty())
                        .or_else(|| {
                            Some(item.item_id).filter(|entry_id| !entry_id.trim().is_empty())
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) async fn interrupt_session_turn(
    state: &ApiState,
    session_id: &SessionId,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
) -> Result<SessionInterruptResponseDto, ApiError> {
    interrupt_session_turn_with_expected(
        state,
        session_id,
        requested_workspace_id,
        requested_workspace_path,
        None,
    )
    .await
}

pub(crate) async fn interrupt_session_turn_with_expected(
    state: &ApiState,
    session_id: &SessionId,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
    expected_turn_id: Option<&str>,
) -> Result<SessionInterruptResponseDto, ApiError> {
    let guard = state.lock_session_turn_commit(session_id).await;
    let mut response = interrupt_session_turn_after_turn_commit(
        state,
        session_id,
        requested_workspace_id,
        requested_workspace_path,
        expected_turn_id,
    )
    .await?;
    drop(guard);
    response.next_queued_turn_started = drain_next_queued_regular_session_turn(
        state.clone(),
        session_id.clone(),
        response.workspace_id.clone().map(WorkspaceId::new),
    )
    .await
        == QueuedRegularSessionTurnDrainOutcome::Started;
    response.remaining_queued_turn_count = state.queued_regular_session_turn_count(session_id);
    Ok(response)
}

pub(super) async fn interrupt_session_turn_after_turn_commit(
    state: &ApiState,
    session_id: &SessionId,
    requested_workspace_id: Option<&str>,
    requested_workspace_path: Option<&str>,
    expected_turn_id: Option<&str>,
) -> Result<SessionInterruptResponseDto, ApiError> {
    let scope = crate::scope_binding::resolve_existing_session_scope(
        state,
        session_id,
        requested_workspace_id,
        requested_workspace_path,
    )?;
    let session = require_session_record_in_scope(state, session_id, &scope)?;
    let session_id = session.session_id.clone();
    let now = UtcMillis::now();
    let workspace_id = state.session_workspace_id(&session);
    let current_turn = state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn);
    let turn_id = current_turn.as_ref().map(|turn| turn.turn_id.clone());
    if expected_turn_id.is_some_and(|expected| turn_id.as_deref() != Some(expected)) {
        return Err(ApiError::turn_conflict(
            "interrupt_target_changed",
            turn_id,
            "停止目标已经变化，未取消新的任务",
        ));
    }
    let active_root_task_id = turn_id
        .as_deref()
        .and_then(|turn_id| session_turn_root_task_id(state, &session_id, turn_id));
    let coordinator_attempt = turn_id.as_deref().and_then(|turn_id| {
        state
            .turn_coordinator()
            .current_attempt(&session_id, turn_id)
            .ok()
    });
    let owned_goal_before_interrupt = turn_id.as_deref().and_then(|turn_id| {
        state
            .session_store
            .active_goal_for_execution_owner(&session_id, turn_id)
    });
    let owns_active_plan_before_interrupt = turn_id.as_deref().is_some_and(|turn_id| {
        state
            .session_store
            .active_plan_for_execution_owner(&session_id, turn_id)
            .is_some()
    });
    let terminal_root_finalized =
        finalize_terminal_root_current_turn(state, &session_id, current_turn.as_ref());
    let interrupted = current_turn
        .as_ref()
        .is_some_and(|turn| turn_status_is_interruptible(&turn.status))
        && !terminal_root_finalized;
    let streaming_entry_ids = if interrupted {
        current_turn_streaming_timeline_entry_ids(state, &session_id)
    } else {
        Vec::new()
    };

    let mut cancelled_tool_process_count = 0;
    let mut agent_settlement_error = None;
    if interrupted || expected_turn_id.is_some() {
        let runner_manager = state.runner_manager();
        let _restart_guard = if let (Some(root_task_id), Some(manager)) =
            (active_root_task_id.as_ref(), runner_manager)
        {
            Some(manager.lock_for_restart(root_task_id.as_str()).await)
        } else {
            None
        };
        let cancelled_item_id = if interrupted {
            state
                .turn_event_sink()
                .interrupt_turn_by_user(&session_id)
                .map_err(|error| ApiError::internal_assembly("中断 session turn 失败", error))?
                .and_then(|sidecar| sidecar.current_turn)
                .and_then(|turn| turn.items.last().map(|item| item.item_id.clone()))
        } else {
            None
        };
        // Turn 已经进入中断终态，后续资源清理必须全部执行；任务树终止失败只记录，
        // 不能短路进程、浏览器租约和协调器的收口。
        if let Some(root_task_id) = active_root_task_id.as_ref()
            && let Some(manager) = runner_manager
            && let Err(error) = manager.kill_execution_scope(root_task_id.as_str())
        {
            tracing::error!(
                session_id = %session_id,
                task_id = %root_task_id,
                %error,
                "中断时终止任务树未完全成功，继续清理执行资源"
            );
        }
        if runner_manager.is_none()
            && let (Some(root), Some(store)) = (active_root_task_id.as_ref(), state.task_store())
            && let Err(error) = store.revoke_active_leases_and_set_execution_subtree_terminal(
                root,
                magi_core::TaskStatus::Killed,
                Vec::new(),
            )
        {
            tracing::error!(%session_id, %root, ?error, "结算替代输入时停止原任务失败");
        }
        cancelled_tool_process_count = if let Some(root) = active_root_task_id.as_ref() {
            state.cancel_task_execution_scope_resources(
                &session_id,
                root,
                magi_browser_authority::BrowserLeaseEndReason::TurnStopped,
            )
        } else {
            state.cancel_conversation_execution_resources(
                &session_id,
                magi_browser_authority::BrowserLeaseEndReason::TurnStopped,
            )
        }
        .total();
        if let Some(root_task_id) = active_root_task_id.as_ref() {
            // 普通停止只等待当前拥有方；Manager 已包含 Dispatcher quiesce，
            // 轻量装配才直接等待 Dispatcher，不能只看 canonical 终态。
            if let Some(manager) = runner_manager {
                manager.quiesce_for_restart(root_task_id.as_str()).await;
            } else if let Some(dispatcher) = state.session_turn_dispatcher() {
                dispatcher.wait_for_quiesce(root_task_id).await;
            }
            state.release_session_git_execution_lease(&session_id);
        }
        // 中断主轮也可能暂停其 Goal；已转交的 Goal 分支服从同一控制版本撤销，
        // Session 分支继续运行，不纳入主轮的资源等待范围。
        agent_settlement_error = state
            .settle_revoked_goal_agent_runs(&session_id)
            .await
            .err();
        if let (Some(dispatcher), Some(turn_id)) =
            (state.session_turn_dispatcher(), turn_id.as_deref())
        {
            dispatcher
                .wait_for_conversation_turn_quiesce(&session_id, turn_id)
                .await;
        }
        if expected_turn_id.is_some() {
            let tasks_unsettled = active_root_task_id
                .as_ref()
                .zip(state.task_store())
                .is_some_and(|(root, store)| {
                    store
                        .collect_execution_subtree_ids(root)
                        .iter()
                        .filter_map(|id| store.get_task(id))
                        .any(|task| {
                            matches!(
                                task.status,
                                magi_core::TaskStatus::Pending | magi_core::TaskStatus::Running
                            )
                        })
                });
            let resource_queries = active_root_task_id
                .as_ref()
                .zip(state.task_store())
                .map(|(root, store)| {
                    store
                        .collect_execution_subtree_ids(root)
                        .into_iter()
                        .map(|task_id| magi_tool_runtime::ToolExecutionContextQuery {
                            session_id: Some(session_id.clone()),
                            task_id: Some(task_id),
                            ..Default::default()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    vec![
                        magi_tool_runtime::ToolExecutionContextQuery::for_taskless_session(
                            &session_id,
                        ),
                    ]
                });
            if tasks_unsettled
                || state.tool_registry().is_some_and(|registry| {
                    resource_queries
                        .iter()
                        .any(|query| registry.has_active_executions(query))
                })
            {
                return Err(ApiError::Conflict(
                    "原执行尚未完成资源结算，替代输入保留待处理；请核对后重试".into(),
                ));
            }
        }
        for entry_id in &streaming_entry_ids {
            state
                .session_store
                .remove_timeline_entry(&session_id, entry_id);
        }
        if let Some(turn_id) = turn_id.as_deref() {
            state
                .turn_coordinator()
                .close_session_turn_input(&session_id, turn_id);
            if let Some(attempt) = coordinator_attempt.as_ref()
                && let Err(error) = state.turn_coordinator().execute_command(
                    &session_id,
                    TurnCommand::Cancel {
                        attempt: attempt.clone(),
                    },
                )
            {
                tracing::error!(
                    session_id = %session_id,
                    turn_id = %attempt.turn_id,
                    %error,
                    "中断后 conversation Coordinator 终态收口失败"
                );
            }
        }
        if let Some(item_id) = cancelled_item_id.as_deref() {
            // 聊天 UI 只接受 canonical turn 事实，interrupt 事件只做运行态通知。
            // Runner/dispatch 已经 quiescent，且 Coordinator 已收口后才发布终态。
            publish_current_session_turn_item_event(
                &state.event_bus,
                &state.session_store,
                &session_id,
                &workspace_id,
                item_id,
                None,
            )
            .map_err(|error| ApiError::internal_assembly("发布中断 Turn 事实失败", error))?;
        }
        if let Some(goal) = owned_goal_before_interrupt.as_ref() {
            if let Some(plan) = state
                .session_store
                .plan(&session_id)
                .filter(|plan| plan.goal_id.as_ref() == Some(&goal.goal_id))
            {
                magi_plan::publish_plan_event(
                    &state.event_bus,
                    magi_plan::plan_event_type(&plan),
                    &plan,
                    workspace_id.as_ref(),
                    None,
                    None,
                );
            }
        } else if owns_active_plan_before_interrupt {
            let plan_store =
                magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
            match plan_store.pause() {
                Ok(Some(plan)) => magi_plan::publish_plan_event(
                    &state.event_bus,
                    magi_plan::plan_event_type(&plan),
                    &plan,
                    workspace_id.as_ref(),
                    None,
                    None,
                ),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    session_id = %session_id,
                    %error,
                    "中断非 Goal 对话后暂停计划失败"
                ),
            }
        }
    }

    state.persist_session_state_checkpoint("session_turn_interrupted")?;
    if let Some(error) = agent_settlement_error {
        return Err(error);
    }
    let event_id = EventId::unique("event-session-turn-interrupt");
    let mut interrupt_payload = json!({
        "session_id": session_id.to_string(),
        "workspace_id": workspace_id.as_ref().map(ToString::to_string),
        "turn_id": turn_id.clone(),
        "interrupted": interrupted,
        "cancelled_tool_process_count": cancelled_tool_process_count,
        "requested_at": now.0,
        "removed_timeline_entry_ids": streaming_entry_ids.clone(),
    });
    append_terminal_canonical_payload(
        &mut interrupt_payload,
        state,
        &session_id,
        turn_id.as_deref(),
    );
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.interrupted",
        interrupt_payload,
    )
    .with_context(EventContext {
        session_id: Some(session_id.clone()),
        workspace_id: workspace_id.clone(),
        ..EventContext::default()
    });
    state.event_bus.publish(event);

    let next_queued_turn_started = false;
    let remaining_queued_turn_count = state.queued_regular_session_turn_count(&session_id);

    Ok(SessionInterruptResponseDto {
        interrupted,
        session_id: session_id.to_string(),
        workspace_id: workspace_id.as_ref().map(ToString::to_string),
        turn_id,
        event_id: event_id.to_string(),
        requested_at: now.0,
        cancelled_tool_process_count,
        remaining_queued_turn_count,
        next_queued_turn_started,
        removed_timeline_entry_ids: streaming_entry_ids,
    })
}

pub(crate) async fn continue_session(
    state: &ApiState,
    request: ContinueSessionRequest,
) -> Result<ContinueSessionResponseDto, ApiError> {
    continue_session_with_expected_root(state, request, None).await
}

pub(crate) async fn continue_session_with_expected_root(
    state: &ApiState,
    request: ContinueSessionRequest,
    expected_root_task_id: Option<&magi_core::TaskId>,
) -> Result<ContinueSessionResponseDto, ApiError> {
    let session_id = parse_session_id(Some(&request.session_id))?;
    let scope = crate::scope_binding::resolve_existing_session_scope(
        state,
        &session_id,
        trimmed_non_empty(request.workspace_id.as_deref()),
        trimmed_non_empty(request.workspace_path.as_deref()),
    )?;
    let workspace_id = scope.workspace_id();
    let session_id = &session_id;
    let workspace_id = &workspace_id;
    // /session/continue 和 agent-run Continue 都绕过普通 turn 分类器，必须在这里
    // 取得同一把 session 锁，才能与新消息、队列 drain 和另一个 Continue 请求串行。
    let _session_turn_guard = state.lock_session_turn_commit(session_id).await;
    if let Some(expected_root) = expected_root_task_id
        && state
            .session_store
            .active_execution_chain(session_id)
            .is_none_or(|chain| &chain.root_task_id != expected_root)
    {
        return Err(ApiError::Conflict(
            "目标根任务已不是当前执行链，请刷新后重试；不能恢复其他主线任务".into(),
        ));
    }
    let requested_prompt_text = request
        .prompt_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let interrupted_recovery_requested = state
        .session_store
        .has_recovery_ready_interruption(session_id);
    let prompt_text = requested_prompt_text
        .or_else(|| interrupted_recovery_requested.then(|| "继续执行中断的任务".to_string()));
    let continued_at = crate::turn_service::monotonic_accepted_at();
    let (request_id, user_message_id) = normalize_session_turn_identity(
        request.request_id,
        request.user_message_id,
        &format!("session-continue-{}-{}", session_id, continued_at.0),
    );
    let placeholder_message_id =
        trimmed_non_empty(request.placeholder_message_id.as_deref()).map(str::to_string);
    let continue_request_fingerprint = magi_conversation_runtime::request_fingerprint(&json!({
        "sessionId": session_id.to_string(),
        "promptText": prompt_text,
        "requestedAgentIds": request.requested_agent_ids,
        "userMessageId": user_message_id,
        "placeholderMessageId": placeholder_message_id,
    }));
    let requested_agent_ids = request
        .requested_agent_ids
        .into_iter()
        .map(|agent_id| agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
        .map(WorkerId::new)
        .collect::<Vec<_>>();
    let resumed_turn_id = format!("turn-session-continue-{}", continued_at.0);
    let (accepted, _) = continue_execution_chain_with_pre_resume(
        state,
        session_id,
        &requested_agent_ids,
        &resumed_turn_id,
        continued_at,
        request_id.clone(),
        continue_request_fingerprint.clone(),
        |chain, primary_branch, branches, coordinator_attempt| {
            let (_, orchestrator_thread_id) =
                state
                    .session_store
                    .ensure_session_mission(session_id, continued_at, || chain.mission_id.clone());
            let accepted_for_turn = SessionContinueAccepted {
                session_id: session_id.clone(),
                mission_id: chain.mission_id.clone(),
                root_task_id: chain.root_task_id.clone(),
                action_task_id: primary_branch.task_id.clone(),
                turn_id: resumed_turn_id.clone(),
                execution_chain_ref: chain.execution_chain_ref.clone(),
                resumed_branch_count: 0,
                runner_started: false,
            };
            write_continue_user_message(ContinueUserMessageInput {
                resumed_thread_ids: branches
                    .iter()
                    .map(|branch| branch.thread_id.clone())
                    .collect(),
                state,
                accepted: &accepted_for_turn,
                prompt_text: prompt_text.as_deref(),
                control_only: false,
                continued_at,
                request_id: Some(request_id.clone()),
                user_message_id: Some(user_message_id.clone()),
                placeholder_message_id: placeholder_message_id.clone(),
                request_fingerprint: Some(continue_request_fingerprint.clone()),
                attempt_id: Some(coordinator_attempt.attempt_id.clone()),
                orchestrator_thread_id,
            })
        },
    )
    .await?;
    finalize_continue_session(state.clone(), accepted.clone());
    state.persist_runtime_durable_state_for_sessions_for_api(std::slice::from_ref(
        &accepted.session_id,
    ))?;
    let event_id = EventId::unique("event-session-continue");
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.continue.executed",
        json!({
            "session_id": accepted.session_id.to_string(),
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "mission_id": accepted.mission_id.to_string(),
            "root_task_id": accepted.root_task_id.to_string(),
            "execution_chain_ref": accepted.execution_chain_ref,
            "resumed_branch_count": accepted.resumed_branch_count,
            "runner_started": accepted.runner_started,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id: workspace_id.clone(),
        mission_id: Some(accepted.mission_id.clone()),
        task_id: Some(accepted.root_task_id.clone()),
        ..EventContext::default()
    });
    let event_sequence = state.event_bus.publish(event);
    Ok(ContinueSessionResponseDto {
        session_id: accepted.session_id.to_string(),
        workspace_id: workspace_id.as_ref().map(ToString::to_string),
        mission_id: accepted.mission_id.to_string(),
        root_task_id: accepted.root_task_id.to_string(),
        execution_chain_ref: accepted.execution_chain_ref,
        resumed_branch_count: accepted.resumed_branch_count,
        status: "continued".to_string(),
        runner_started: accepted.runner_started,
        event_id: event_id.to_string(),
        continued_at,
        turn_id: accepted.turn_id,
        request_id,
        execution_profile: ExecutionProfile::Task,
        event_sequence,
    })
}

pub(crate) fn cancel_active_session_turn_for_lifecycle(
    state: &ApiState,
    session_id: &SessionId,
) -> bool {
    let cancelled_tool_process_count = state
        .cancel_execution_resources(
            Some(session_id),
            None,
            None,
            magi_browser_authority::BrowserLeaseEndReason::SessionClosed,
        )
        .total();
    let current_turn = state
        .session_store
        .runtime_sidecar(session_id)
        .and_then(|sidecar| sidecar.current_turn);
    let coordinator_attempt = current_turn.as_ref().and_then(|turn| {
        state
            .turn_coordinator()
            .current_attempt(session_id, &turn.turn_id)
            .ok()
    });
    let Some(current_turn) = current_turn.filter(|turn| turn_status_is_interruptible(&turn.status))
    else {
        return cancelled_tool_process_count > 0;
    };
    let owns_active_plan = state
        .session_store
        .active_plan_for_execution_owner(session_id, &current_turn.turn_id)
        .is_some();
    match state.turn_event_sink().cancel_turn(session_id) {
        Ok(Some(_)) => {}
        Ok(None) => return cancelled_tool_process_count > 0,
        Err(error) => {
            tracing::error!(
                %session_id,
                %error,
                "关闭会话时取消当前 Turn 失败"
            );
            return false;
        }
    }
    state
        .turn_coordinator()
        .close_session_turn_input(session_id, &current_turn.turn_id);
    if let Some(attempt) = coordinator_attempt.as_ref()
        && let Err(error) = state.turn_coordinator().execute_command(
            session_id,
            TurnCommand::Cancel {
                attempt: attempt.clone(),
            },
        )
    {
        tracing::error!(
            %session_id,
            turn_id = %attempt.turn_id,
            %error,
            "关闭会话后 Coordinator 取消收口失败"
        );
    }
    if !owns_active_plan {
        return true;
    }
    let plan_store = magi_plan::PlanStore::new(state.session_store.clone(), session_id.clone());
    let workspace_id = state
        .session_store
        .session(session_id)
        .and_then(|session| session.workspace_id)
        .map(magi_core::WorkspaceId::new);
    match plan_store.pause() {
        Ok(Some(plan)) => magi_plan::publish_plan_event(
            &state.event_bus,
            magi_plan::plan_event_type(&plan),
            &plan,
            workspace_id.as_ref(),
            None,
            None,
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(session_id = %session_id, %error, "关闭会话后暂停计划失败"),
    }
    true
}
