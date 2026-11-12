use super::*;

pub(super) async fn submit_steer_current_turn(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    scope: &SessionScope,
    accepted_at: UtcMillis,
    request_fingerprint: &str,
) -> Result<SessionTurnResponseDto, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    // 引导不进入排队，只需按 canonical 事实判重；必须在锁内判断，否则并发重复的
    // 引导会被模型看到两次。
    if let Some(replay) = replay_existing_submission(state, request, request_fingerprint, false)? {
        return Ok(replay);
    }
    check_input_guard(state, request, "steer").await?;
    submit_steer_current_turn_after_turn_commit(
        state,
        request,
        scope,
        accepted_at,
        request_fingerprint,
        None,
    )
    .await
}

/// 征询扩展 `input` guard。被阻止的输入在写入任何会话、Turn 或队列事实之前就被拒绝。
pub(super) async fn check_input_guard(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    kind: &'static str,
) -> Result<(), ApiError> {
    use magi_conversation_runtime::extension_hooks::{InputDecision, InputView};
    let Some(guard) = state.input_guard.as_ref() else {
        return Ok(());
    };
    let session_id = request.requested_session_id().map(|id| id.to_string());
    let workspace_id = request.requested_workspace_id();
    let request_id = request.request_id();
    let decision = guard
        .check(InputView {
            session_id: session_id.as_deref(),
            workspace_id: workspace_id.as_deref(),
            request_id: request_id.as_deref(),
            text: request.text.as_deref().unwrap_or_default(),
            image_count: request.images.len(),
            kind,
        })
        .await;
    match decision {
        InputDecision::Proceed => Ok(()),
        InputDecision::Blocked { plugin, reason } => Err(ApiError::Forbidden(format!(
            "扩展 {plugin} 阻止了该输入：{reason}"
        ))),
    }
}

/// 调用方已持有会话提交锁。指纹描述原始输入创建，队列改为 steer 不改变它。
pub(crate) async fn submit_steer_current_turn_after_turn_commit(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    scope: &SessionScope,
    accepted_at: UtcMillis,
    creation_request_fingerprint: &str,
    plugin_message: Option<&magi_app_server_protocol::generated::PluginSessionMessageOrigin>,
) -> Result<SessionTurnResponseDto, ApiError> {
    let session_id = parse_session_id(request.session_id.as_deref())?;
    require_session_record_in_scope(state, &session_id, scope)?;
    let expected_turn_id = request
        .expected_turn_id()
        .ok_or_else(|| ApiError::InvalidInput("引导当前回复必须提供 expectedTurnId".to_string()))?;
    // steer 仍属于当前 Turn 的同一个 execution attempt；先由 Coordinator 校验
    // 当前所有权，再在同一 session lock 内写入 canonical user item 和 Coordinator FIFO。
    let coordinator_attempt = state
        .turn_coordinator()
        .current_attempt(&session_id, &expected_turn_id)
        .map_err(|error| match error {
            magi_conversation_runtime::CoordinatorError::TurnMismatch { expected, .. } => {
                ApiError::turn_conflict(
                    "expected_turn_mismatch",
                    Some(expected),
                    "当前回复已经结束或已切换，请基于最新 Turn 重新引导",
                )
            }
            magi_conversation_runtime::CoordinatorError::NoActiveTurn => {
                ApiError::turn_conflict("no_active_turn", None, "当前回复已经结束，无法继续引导")
            }
            other => ApiError::Conflict(other.to_string()),
        })?;
    validate_steer_directory_binding(state, &session_id, &expected_turn_id, request)?;
    if !session_turn_request_is_steerable_input(request) {
        return Err(ApiError::InvalidInput(
            "引导仅支持文字与图片，不能混合命令或其他上下文选择".to_string(),
        ));
    }
    if !session_executor_accepts_steer(state, &session_id, &expected_turn_id, request) {
        return Err(ApiError::turn_conflict(
            "executor_steer_unsupported",
            Some(expected_turn_id.clone()),
            "当前执行器不支持这类运行中补充，请保留排队并在下一轮发送",
        ));
    }
    let message = request.trimmed_text().unwrap_or_default();
    let images = request.parsed_images().map_err(ApiError::InvalidInput)?;
    let orchestrator_thread_id = state
        .session_store
        .orchestrator_thread_for_session(&session_id)
        .map(|thread| thread.thread_id)
        .ok_or_else(|| ApiError::InvalidInput("当前会话没有主线执行线程".to_string()))?;
    let entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let request_id = Some(
        request
            .request_id()
            .unwrap_or_else(|| format!("request-steer-{}-{}", session_id, accepted_at.0)),
    );
    let user_message_id = Some(
        request
            .user_message_id()
            .unwrap_or_else(|| format!("user-steer-{}-{}", session_id, accepted_at.0)),
    );
    state
        .turn_coordinator()
        .execute_command(
            &session_id,
            TurnCommand::Steer {
                attempt: coordinator_attempt.clone(),
                request_id: request_id
                    .clone()
                    .expect("steer request id should be normalized"),
            },
        )
        .map_err(|error| match error {
            magi_conversation_runtime::CoordinatorError::SteerUnsupported { .. } => {
                ApiError::turn_conflict(
                    "steer_unsupported",
                    Some(expected_turn_id.clone()),
                    "当前回复已关闭引导输入，请等待下一轮执行",
                )
            }
            other => ApiError::Conflict(format!("steer Turn 命令校验失败: {other}")),
        })?;
    let (user_message_item_id, mut user_message_item) =
        build_user_message_turn_item(UserMessageTurnItemInput {
            accepted_at,
            message: &message,
            entry_id: &entry_id,
            request_id: request_id.clone(),
            user_message_id: user_message_id.clone(),
            placeholder_message_id: None,
            metadata: {
                let mut metadata = std::collections::HashMap::from([
                    (
                        "route".to_string(),
                        serde_json::Value::String("steer".to_string()),
                    ),
                    (
                        "requestFingerprint".to_string(),
                        serde_json::Value::String(creation_request_fingerprint.to_string()),
                    ),
                    (
                        "executionProfile".to_string(),
                        serde_json::Value::String(coordinator_attempt.profile.to_string()),
                    ),
                    (
                        "requestId".to_string(),
                        serde_json::Value::String(request_id.clone().expect("steer request id")),
                    ),
                ]);
                if let Some(origin) = plugin_message {
                    metadata.insert(
                        "pluginMessage".into(),
                        serde_json::to_value(origin).expect("origin serializes"),
                    );
                }
                metadata.extend(
                    magi_conversation_runtime::session_images::session_turn_images_metadata(
                        &images,
                    ),
                );
                metadata
            },
            task_id: None,
            source_thread_id: orchestrator_thread_id,
        });
    user_message_item.item_seq = 0;
    let signal = UserSignal {
        text: Some(match plugin_message {
            Some(origin) => magi_session_store::plugin_message_model_content(
                &message,
                &serde_json::to_value(origin).expect("origin serializes"),
            ),
            None => message,
        }),
        images: magi_conversation_runtime::session_images::session_turn_image_sources(&images),
        request_id: request_id.clone(),
        user_message_id: user_message_id.clone(),
        placeholder_message_id: None,
        accepted_at,
    };
    state
        .turn_coordinator()
        .try_steer_session_turn_with(&session_id, &expected_turn_id, signal, || {
            state
                .turn_event_sink()
                .upsert_item_sidecar(&session_id, Some(&expected_turn_id), user_message_item)
                .and_then(|sidecar| {
                    sidecar.ok_or(magi_core::DomainError::InvalidState {
                        message: "当前会话没有可写入的活跃 Turn".to_string(),
                    })
                })
        })
        .map_err(|error| match error {
            SessionTurnInputCommitError::Input(input_error) => steer_input_error(input_error),
            SessionTurnInputCommitError::Commit(error) => {
                ApiError::internal_assembly("写入当前 Turn 引导失败", error)
            }
        })?;
    state.persist_session_state_checkpoint("session_turn_steered")?;
    publish_current_session_turn_item_event(
        state.event_bus.as_ref(),
        state.session_store.as_ref(),
        &session_id,
        &scope.workspace_id(),
        &user_message_item_id,
        state.task_store(),
    )
    .map_err(|error| ApiError::internal_assembly("发布引导用户消息事实失败", error))?;
    let canonical_turn = state
        .session_store
        .canonical_turns_for_session(&session_id)
        .into_iter()
        .find(|turn| turn.turn_id == expected_turn_id);
    let canonical_item = canonical_turn
        .as_ref()
        .and_then(|turn| {
            turn.items
                .iter()
                .find(|item| item.item_id == user_message_item_id)
        })
        .cloned();

    let event_id = EventId::unique(format!("event-session-turn-steered-{}", session_id));
    let event_sequence = state.event_bus.publish(
        EventEnvelope::domain(
            event_id.clone(),
            "session.turn.steered",
            json!({
                "session_id": session_id,
                "turn_id": expected_turn_id,
                "request_id": request_id,
                "user_message_id": user_message_id,
                "execution_profile": coordinator_attempt.profile,
                "accepted_at": accepted_at.0,
                "canonical_turn": canonical_turn,
                "canonical_item": canonical_item,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id: scope.workspace_id(),
            ..EventContext::default()
        }),
    );

    Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id,
        entry_id,
        event_id: event_id.clone(),
        accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: state.event_bus.snapshot().next_sequence,
        created_session: false,
        route: SessionTurnRouteDto::Steer,
        root_task_id: None,
        action_task_id: None,
        execution_chain_ref: None,
        user_message_item_id: Some(user_message_item_id),
    })
    .with_steered_turn(expected_turn_id)
    .with_canonical_event("turn_item_upsert", canonical_turn, canonical_item)
    .with_canonical_event_metadata(event_id, event_sequence, accepted_at)
    .with_request_identity(request_id, event_sequence))
}

/// 补充属于原 Turn；显式目录选择和排队时的默认目录都不能把原执行重定位或扩权。
pub(super) fn validate_steer_directory_binding(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: &str,
    request: &SessionTurnRequestDto,
) -> Result<(), ApiError> {
    let Some(turn) = state
        .session_store
        .canonical_turn_for_session_turn_id(session_id, turn_id)
    else {
        return Err(ApiError::turn_conflict(
            "no_active_turn",
            None,
            "当前回复已经结束",
        ));
    };
    let binding = crate::task_dispatch::canonical_project_directory_binding(&turn)
        .map_err(|error| ApiError::internal_assembly("读取原 Turn 目录绑定", error))?;
    if let Some(binding) = binding.as_ref() {
        state
            .workspace_registry
            .validate_directory_membership(binding)
            .map_err(|error| {
                ApiError::Conflict(format!("原 Turn 的源文件夹授权已失效: {error}"))
            })?;
    }
    let selected = request
        .project_directories
        .as_ref()
        .map(|binding| binding.default_source_folder_id.as_str())
        .or_else(|| trimmed_non_empty(request.source_folder_id.as_deref()));
    if selected.is_some_and(|id| {
        binding
            .as_ref()
            .is_none_or(|binding| binding.default_source_folder_id != id)
    }) || request.project_directories.as_ref().is_some_and(|queued| {
        binding.as_ref().is_none_or(|original| {
            queued.workspace_id != original.workspace_id
                || queued
                    .source_folders
                    .iter()
                    .any(|grant| !original.source_folders.contains(grant))
        })
    }) {
        return Err(ApiError::InvalidInput(
            "补充的源文件夹与当前任务不同，请保留到下一轮执行".into(),
        ));
    }
    Ok(())
}

/// 同一份输入能力用于队列展示；提交仍通过 Coordinator 在锁内校验目标和输入边界。
pub(crate) fn session_turn_guide_capability(
    state: &ApiState,
    session_id: &SessionId,
    request: &SessionTurnRequestDto,
) -> magi_app_server_protocol::generated::SessionTurnGuideCapability {
    use magi_app_server_protocol::generated::{
        SessionTurnGuideCapability, SessionTurnGuideEffectiveAt, SessionTurnGuideInputSupport,
        SessionTurnGuideStatus,
    };
    let coordinator = state.turn_coordinator();
    let target_turn_id = coordinator.active_turn_id(session_id);
    let status = if !session_turn_request_is_steerable_input(request) {
        SessionTurnGuideStatus::UnsupportedInput
    } else if target_turn_id.is_none() {
        SessionTurnGuideStatus::NoActiveTurn
    } else if validate_steer_directory_binding(
        state,
        session_id,
        target_turn_id.as_deref().expect("active turn"),
        request,
    )
    .is_err()
    {
        SessionTurnGuideStatus::UnsupportedInput
    } else if !session_executor_accepts_steer(
        state,
        session_id,
        target_turn_id.as_deref().expect("active turn"),
        request,
    ) {
        SessionTurnGuideStatus::UnsupportedExecutor
    } else if !coordinator.active_turn_accepts_steer(session_id) {
        SessionTurnGuideStatus::InputClosed
    } else {
        SessionTurnGuideStatus::Ready
    };
    let channel_ready = matches!(status, SessionTurnGuideStatus::Ready);
    let images_supported = channel_ready
        && !request.images.is_empty()
        && session_executor_accepts_steer(
            state,
            session_id,
            target_turn_id
                .as_deref()
                .expect("ready guide has active turn"),
            request,
        );
    SessionTurnGuideCapability {
        status,
        target_turn_id,
        effective_at: Some(if channel_ready {
            SessionTurnGuideEffectiveAt::NextModelStep
        } else {
            SessionTurnGuideEffectiveAt::Unavailable
        }),
        input_support: Some(SessionTurnGuideInputSupport {
            text: channel_ready,
            images: images_supported,
            context_references: false,
            browser_annotation_refs: false,
        }),
    }
}

/// 运行中使用已冻结的主线 Step 合同；发送前对尚未声明的插件引擎保守关闭引导。
pub(super) fn session_executor_accepts_steer(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: &str,
    request: &SessionTurnRequestDto,
) -> bool {
    let frozen = state
        .session_store
        .canonical_turn_for_session_turn_id(session_id, turn_id)
        .and_then(|turn| {
            turn.model_steps
                .into_iter()
                .rev()
                .filter(|step| {
                    step.purpose == Some(magi_session_store::ModelStepPurpose::Execution)
                })
                .find(|step| {
                    step.task_id.as_ref().is_none_or(|task_id| {
                        state
                            .task_store()
                            .and_then(|store| store.get_task(task_id))
                            .is_some_and(|task| task.parent_task_id.is_none())
                    })
                })
        })
        .and_then(|step| step.executor_capabilities);
    if let Some(caps) = frozen {
        return caps.steer != magi_bridge_client::ExecutorSteerMode::None
            && (request.images.is_empty()
                || caps
                    .input_modalities
                    .contains(&magi_bridge_client::ExecutorInputModality::Image));
    }
    true
}

pub(crate) fn session_turn_request_is_steerable_input(request: &SessionTurnRequestDto) -> bool {
    request.interrupt_turn_id.is_none()
        && request.command.is_none()
        && request.plugin_command_id.is_none()
        && (request.trimmed_text().is_some() || !request.images.is_empty())
        && request
            .skill_name
            .as_deref()
            .is_none_or(|skill_name| skill_name.trim().is_empty())
        && !request.goal_mode
        && request.context_references.is_empty()
        && request.browser_annotation_refs.is_empty()
        && request.browser_node_selections.is_empty()
}

pub(super) fn steer_input_error(error: SessionTurnInputError) -> ApiError {
    match error {
        SessionTurnInputError::NoActiveTurn => {
            ApiError::turn_conflict("no_active_turn", None, "当前回复已经结束，无法继续引导")
        }
        SessionTurnInputError::TurnMismatch { active_turn_id, .. } => ApiError::turn_conflict(
            "expected_turn_mismatch",
            Some(active_turn_id),
            "当前回复已经切换，请基于最新状态重新发送",
        ),
        SessionTurnInputError::AlreadyActive { active_turn_id } => ApiError::turn_conflict(
            "channel_already_active",
            Some(active_turn_id),
            "当前会话的引导通道状态冲突",
        ),
    }
}
