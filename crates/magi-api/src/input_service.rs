//! 持久 Input 队列应用服务。HTTP 与 App Server 只适配传输，共用此处提交锁与状态转换。
use crate::scope_binding::{
    SessionScope, parse_session_id, require_session_record_in_scope,
    resolve_existing_session_scope, resolve_explicit_session_scope,
};
use crate::turn_service::{
    session_turn_guide_capability, session_turn_request_is_steerable_input,
    submit_steer_current_turn_after_turn_commit, trimmed_non_empty,
};
use crate::{errors::ApiError, state::ApiState};
use magi_app_server_protocol::generated::QueuedSessionTurn as QueuedSessionTurnDto;
pub(crate) use magi_app_server_protocol::generated::{
    SessionInteractionScope as SessionTurnQueueScope,
    SessionQueueGuideParams as GuideSessionTurnQueueItemRequest,
    SessionQueueItemParams as SessionTurnQueueItemRequest,
    SessionQueueSnapshot as SessionTurnQueueResponseDto,
};
use magi_core::SessionId;

fn wire_projection<T: serde::de::DeserializeOwned>(value: impl serde::Serialize) -> T {
    serde_json::from_value(serde_json::to_value(value).expect("Input projection serializes"))
        .expect("validated Input fields match wire schema")
}

pub(crate) fn session_turn_queue_response(
    state: &ApiState,
    session_id: &SessionId,
) -> SessionTurnQueueResponseDto {
    let (inputs, through_sequence) = state.session_store.inbox_snapshot(session_id);
    let queued_turns = state
        .queued_regular_session_turns(session_id)
        .into_iter()
        .enumerate()
        .map(|(index, queued)| {
            let text = queued.request.trimmed_text();
            let guide = session_turn_guide_capability(state, session_id, &queued.request);
            let interruption_error = inputs
                .iter()
                .find(|input| input.input_id == queued.queue_id)
                .and_then(|input| input.interruption.as_ref())
                .and_then(|interruption| interruption.error.clone());
            QueuedSessionTurnDto {
                revision: inputs
                    .iter()
                    .find(|input| input.input_id == queued.queue_id)
                    .expect("pending input exists")
                    .revision,
                queue_id: queued.queue_id,
                queue_position: (index + 1) as u64,
                request_id: queued.request.request_id(),
                session_id: queued.session_id.to_string(),
                workspace_id: queued
                    .requested_workspace_id
                    .as_ref()
                    .map(ToString::to_string),
                workspace_path: queued.request.workspace_path.clone(),
                accepted_at: queued.accepted_at.0,
                content: queued.request.timeline_content(text.as_deref()),
                text,
                command: queued.request.command.clone(),
                workflow_id: queued.request.workflow_id.clone(),
                plugin_command_id: queued.request.plugin_command_id.clone(),
                skill_name: queued.request.skill_name.clone(),
                goal_mode: queued.request.goal_mode,
                access_profile: wire_projection(queued.request.access_profile),
                images: wire_projection(queued.request.images),
                context_references: wire_projection(queued.request.context_references),
                browser_annotation_refs: queued.request.browser_annotation_refs,
                browser_node_selections: wire_projection(queued.request.browser_node_selections),
                plugin_selections: queued.request.plugin_selections,
                guide,
                retry_count: queued.retry_count,
                interrupt_turn_id: queued.request.interrupt_turn_id,
                interruption_error,
            }
        })
        .collect();
    SessionTurnQueueResponseDto {
        cursor: magi_app_server_protocol::generated::SessionInboxCursor {
            epoch: format!(
                "{}:{}",
                session_id,
                state
                    .session_store
                    .session(session_id)
                    .expect("scoped session exists")
                    .created_at
                    .0
            ),
            // Input revision 每次提交递增，保留终态记录，重启和重连后仍单调。
            through_sequence,
        },
        session_id: session_id.to_string(),
        queued_turns,
    }
}

fn require_session_turn_queue_scope(
    state: &ApiState,
    scope: &SessionTurnQueueScope,
) -> Result<(SessionId, SessionScope), ApiError> {
    let session_id = parse_session_id(Some(&scope.session_id))?;
    let session_scope = resolve_existing_session_scope(
        state,
        &session_id,
        scope
            .workspace_id
            .as_deref()
            .and_then(|value| trimmed_non_empty(Some(value))),
        scope
            .workspace_path
            .as_deref()
            .and_then(|value| trimmed_non_empty(Some(value))),
    )?;
    Ok((session_id, session_scope))
}

fn validate_input_revision(expected: Option<u64>) -> Result<(), ApiError> {
    if expected == Some(0) {
        return Err(ApiError::InvalidInput(
            "队列变更需要当前输入的 expectedRevision".into(),
        ));
    }
    Ok(())
}

fn require_input_revision(
    state: &ApiState,
    session_id: &SessionId,
    queue_id: &str,
    expected: Option<u64>,
) -> Result<(), ApiError> {
    validate_input_revision(expected)?;
    if let Some(expected) = expected {
        let input = state
            .session_store
            .input_for_id(session_id, queue_id)
            .ok_or_else(|| ApiError::not_found("输入不存在", queue_id))?;
        if input.revision != expected
            || input.state != magi_session_store::SessionInputState::Pending
        {
            return Err(ApiError::Conflict(
                "输入已被领取、编辑或撤销，请刷新待处理消息".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn get_session_turn_queue(
    state: ApiState,
    scope: SessionTurnQueueScope,
) -> Result<SessionTurnQueueResponseDto, ApiError> {
    let (session_id, session_scope) = require_session_turn_queue_scope(&state, &scope)?;
    // 与接纳、引导、删除共用提交锁，返回完整已提交 Inbox 及当前能力。
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    require_session_record_in_scope(&state, &session_id, &session_scope)?;
    Ok(session_turn_queue_response(&state, &session_id))
}

/// 只改原 pending Input 的内容；创建指纹、顺序和用户 item 身份保持原提交事实。
pub(crate) async fn update_session_turn_queue_item(
    state: ApiState,
    request: magi_app_server_protocol::generated::SessionQueueUpdateParams,
) -> Result<SessionTurnQueueResponseDto, ApiError> {
    let (session_id, session_scope) = require_session_turn_queue_scope(
        &state,
        &SessionTurnQueueScope {
            session_id: request.session_id,
            workspace_id: request.workspace_id,
            workspace_path: request.workspace_path,
        },
    )?;
    let _guard = state.lock_session_turn_commit(&session_id).await;
    require_session_record_in_scope(&state, &session_id, &session_scope)?;
    let mut input = state
        .session_store
        .input_for_id(&session_id, request.queue_id.trim())
        .ok_or_else(|| ApiError::not_found("排队输入不存在", &request.queue_id))?;
    if input.state != magi_session_store::SessionInputState::Pending || input.interruption.is_some()
    {
        return Err(ApiError::Conflict(
            "输入已被领取、撤销或属于停止并改做，不能编辑".into(),
        ));
    }
    validate_input_revision(Some(request.expected_revision))?;
    if state
        .session_store
        .session(&session_id)
        .is_none_or(|session| session.status != magi_core::SessionLifecycleStatus::Active)
    {
        return Err(ApiError::Conflict("输入版本无效或会话已经关闭".into()));
    }
    let mut queued: crate::state::QueuedRegularSessionTurn =
        serde_json::from_value(input.payload.clone())
            .map_err(|error| ApiError::internal_assembly("读取排队输入", error))?;
    let replacement: crate::dto::SessionTurnRequestDto = serde_json::from_value(
        serde_json::to_value(request.request).expect("wire request serializes"),
    )
    .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    let original_scope = resolve_existing_session_scope(
        &state,
        &session_id,
        queued.request.requested_workspace_id().as_deref(),
        queued.request.requested_workspace_path().as_deref(),
    )?;
    let replacement_scope = resolve_explicit_session_scope(
        &state,
        replacement.scope,
        replacement.requested_workspace_id().as_deref(),
        replacement.requested_workspace_path().as_deref(),
    )?;
    if original_scope != replacement_scope
        || replacement.requested_session_id().as_ref() != Some(&session_id)
        || replacement.resume
        || replacement.steer_current_turn
        || replacement.expected_turn_id.is_some()
        || replacement.replace_turn_id.is_some()
        || replacement.interrupt_turn_id.is_some()
        || replacement.request_id.is_some()
        || replacement.user_message_id.is_some()
        || replacement.placeholder_message_id.is_some()
        || replacement.orchestrator_session_config.is_some()
    {
        return Err(ApiError::InvalidInput(
            "编辑只能修改原输入内容，不能改变会话、模型或执行身份".into(),
        ));
    }
    let candidate = &mut queued.request;
    candidate.text = replacement.text;
    candidate.images = replacement.images;
    candidate.context_references = replacement.context_references;
    candidate.browser_annotation_refs = replacement.browser_annotation_refs;
    candidate.browser_node_selections = replacement.browser_node_selections;
    candidate.plugin_selections = replacement.plugin_selections;
    candidate.skill_name = replacement.skill_name;
    candidate.goal_mode = replacement.goal_mode;
    candidate.command = replacement.command;
    candidate.workflow_id = replacement.workflow_id;
    candidate.plugin_command_id = replacement.plugin_command_id;
    candidate.access_profile = replacement.access_profile;
    candidate.locale = replacement.locale;
    crate::turn_service::validate_session_turn_input(candidate)?;
    candidate
        .validate_context_references()
        .map_err(ApiError::InvalidInput)?;
    candidate
        .validate_browser_node_selections()
        .map_err(ApiError::InvalidInput)?;
    candidate.parsed_images().map_err(ApiError::InvalidInput)?;
    crate::turn_service::validate_workflow_selection(&state, candidate)?;
    crate::turn_service::validate_plugin_command_selection(&state, candidate)?;
    let decision = crate::turn_service::decide_session_turn(&state, candidate)?;
    queued.route = decision.route;
    queued.task_title = decision.task_title;
    queued.execution_goal = decision.execution_goal;
    queued.required_tool_chain = decision.required_tool_chain;
    queued.completion_contract = decision.completion_contract;
    queued.recovery_checkpoint = decision.recovery_checkpoint;
    let payload = serde_json::to_value(queued)
        .map_err(|error| ApiError::internal_assembly("保存排队输入", error))?;
    if input.revision != request.expected_revision {
        // 同一次保存的响应丢失后，可读取已经提交的完全相同内容；不再次修改 revision。
        if request.expected_revision.checked_add(1) == Some(input.revision)
            && input.payload == payload
        {
            return Ok(session_turn_queue_response(&state, &session_id));
        }
        return Err(ApiError::Conflict(
            "输入已被编辑，请刷新后核对原消息".into(),
        ));
    }
    input.revision = input
        .revision
        .checked_add(1)
        .ok_or_else(|| ApiError::Conflict("输入版本已溢出".into()))?;
    input.payload = payload;
    state
        .session_store
        .commit_input(input, Some(request.expected_revision))
        .map_err(|error| ApiError::internal_assembly("提交输入编辑失败", error))?;
    Ok(session_turn_queue_response(&state, &session_id))
}

pub(crate) async fn remove_session_turn_queue_item(
    state: ApiState,
    request: SessionTurnQueueItemRequest,
) -> Result<SessionTurnQueueResponseDto, ApiError> {
    let (session_id, session_scope) = require_session_turn_queue_scope(
        &state,
        &SessionTurnQueueScope {
            session_id: request.session_id.clone(),
            workspace_id: request.workspace_id.clone(),
            workspace_path: request.workspace_path.clone(),
        },
    )?;
    let queue_id = request.queue_id.trim();
    if queue_id.is_empty() {
        return Err(ApiError::InvalidInput("queueId 不能为空".to_string()));
    }
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    require_session_record_in_scope(&state, &session_id, &session_scope)?;
    require_input_revision(&state, &session_id, queue_id, request.expected_revision)?;
    if let Some(queued) = state
        .queued_regular_session_turns(&session_id)
        .into_iter()
        .find(|queued| queued.queue_id == queue_id)
    {
        crate::turn_service::settle_queued_input_interruption_after_turn_commit(
            &state,
            &session_id,
            &queued,
        )
        .await?;
    }
    state.remove_regular_session_turn(&session_id, queue_id)?;
    // 删除本身不留下独立调度器；让仍在等待的普通 FIFO 沿既有入口继续。
    crate::turn_service::schedule_next_queued_regular_session_turn(
        state.clone(),
        session_id.clone(),
        state
            .session_store
            .session(&session_id)
            .and_then(|session| state.session_workspace_id(&session)),
    );
    Ok(session_turn_queue_response(&state, &session_id))
}

pub(crate) async fn retry_session_turn_queue_item(
    state: ApiState,
    request: SessionTurnQueueItemRequest,
) -> Result<SessionTurnQueueResponseDto, ApiError> {
    let (session_id, scope) = require_session_turn_queue_scope(
        &state,
        &SessionTurnQueueScope {
            session_id: request.session_id.clone(),
            workspace_id: request.workspace_id.clone(),
            workspace_path: request.workspace_path.clone(),
        },
    )?;
    let guard = state.lock_session_turn_commit(&session_id).await;
    require_session_record_in_scope(&state, &session_id, &scope)?;
    if request.expected_revision.is_none() {
        return Err(ApiError::InvalidInput("重试需要 expectedRevision".into()));
    }
    require_input_revision(
        &state,
        &session_id,
        request.queue_id.trim(),
        request.expected_revision,
    )?;
    state
        .session_store
        .input_for_id(&session_id, request.queue_id.trim())
        .filter(|input| {
            input.state == magi_session_store::SessionInputState::Pending
                && input.interruption.is_some()
        })
        .ok_or_else(|| ApiError::Conflict("只能重试尚未领取的停止并改做输入".into()))?;
    // 不创建新输入、不改投递目标；重新进入原结算与接纳入口。
    let snapshot = session_turn_queue_response(&state, &session_id);
    drop(guard);
    crate::turn_service::schedule_next_queued_regular_session_turn(
        state,
        session_id,
        scope.workspace_id(),
    );
    Ok(snapshot)
}

pub(crate) async fn guide_session_turn_queue_item(
    state: ApiState,
    request: GuideSessionTurnQueueItemRequest,
) -> Result<SessionTurnQueueResponseDto, ApiError> {
    let (session_id, scope) = require_session_turn_queue_scope(
        &state,
        &SessionTurnQueueScope {
            session_id: request.session_id.clone(),
            workspace_id: request.workspace_id.clone(),
            workspace_path: request.workspace_path.clone(),
        },
    )?;
    let queue_id = request.queue_id.trim();
    if queue_id.is_empty() {
        return Err(ApiError::InvalidInput("queueId 不能为空".to_string()));
    }

    validate_input_revision(request.expected_revision)?;
    let expected_turn_id = trimmed_non_empty(Some(&request.expected_turn_id))
        .ok_or_else(|| ApiError::InvalidInput("补充当前任务必须提供 expectedTurnId".to_string()))?;

    // 领取、删除和改投递共用同一临界区；用户 item 与 Input 绑定由同一事务提交。
    let _session_turn_guard = state.lock_session_turn_commit(&session_id).await;
    require_session_record_in_scope(&state, &session_id, &scope)?;
    if let Some(input) = state
        .session_store
        .inputs_for_session(&session_id)
        .iter()
        .find(|input| {
            input.input_id == queue_id
                && matches!(
                    input.state,
                    magi_session_store::SessionInputState::Attached
                        | magi_session_store::SessionInputState::Consumed
                )
        })
    {
        if input.attached_turn_id.as_deref() != Some(expected_turn_id) {
            return Err(ApiError::turn_conflict(
                "expected_turn_mismatch",
                input.attached_turn_id.clone(),
                "这条输入已绑定其他回复",
            ));
        }
        // 绑定和首次消费各增加一个版本；仅同一次改投递可重读成功回执。
        let revision_advance = if input.state == magi_session_store::SessionInputState::Consumed {
            2
        } else {
            1
        };
        let was_guided = state
            .session_store
            .canonical_turn_for_session_turn_id(&session_id, expected_turn_id)
            .is_some_and(|turn| {
                turn.items.iter().any(|item| {
                    item.item_id == input.user_message_id
                        && item.metadata.get("route").and_then(|value| value.as_str())
                            == Some("steer")
                })
            });
        if request
            .expected_revision
            .is_some_and(|expected| expected.checked_add(revision_advance) != Some(input.revision))
            || !was_guided
        {
            return Err(ApiError::Conflict(
                "输入已被领取或编辑，请刷新待处理消息".into(),
            ));
        }
        return Ok(session_turn_queue_response(&state, &session_id));
    }
    require_input_revision(&state, &session_id, queue_id, request.expected_revision)?;
    let queued = state
        .queued_regular_session_turns(&session_id)
        .into_iter()
        .find(|turn| turn.queue_id == queue_id)
        .ok_or_else(|| ApiError::not_found("排队消息不存在", queue_id))?;

    {
        if !session_turn_request_is_steerable_input(&queued.request) {
            return Err(ApiError::InvalidInput(
                "补充当前任务仅支持文字与图片，其他输入请留在下一轮发送".to_string(),
            ));
        }
        let mut steer_request = queued.request.clone();
        steer_request.session_id = Some(session_id.to_string());
        steer_request.workspace_id = scope.workspace_id().as_ref().map(ToString::to_string);
        steer_request.steer_current_turn = true;
        steer_request.expected_turn_id = Some(expected_turn_id.to_string());
        // 改投递目标不改变原始输入的创建身份；原始提交响应丢失后仍能正确重放。
        let creation_fingerprint = queued
            .request_fingerprint
            .clone()
            .map(Ok)
            .unwrap_or_else(|| queued.request.request_fingerprint())
            .map_err(ApiError::InvalidInput)?;
        submit_steer_current_turn_after_turn_commit(
            &state,
            &steer_request,
            &scope,
            crate::turn_service::monotonic_accepted_at(),
            &creation_fingerprint,
        )
        .await?;
    }

    Ok(session_turn_queue_response(&state, &session_id))
}
