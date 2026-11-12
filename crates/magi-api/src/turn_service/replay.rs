use super::*;

/// requestId 幂等的唯一判定：返回同一请求已有的 canonical Turn 或排队位置。
///
/// 必须在持有 session Turn 锁（新会话为导航锁）之后调用：锁外判断会让两个并发的相同
/// 请求同时通过，产生重复 Turn 或重复引导。canonical accepted 事实优先，daemon 重启或
/// 响应丢失后同一 requestId 仍返回原 Turn。
pub(super) fn replay_existing_submission(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    request_fingerprint: &str,
    include_queue: bool,
) -> Result<Option<SessionTurnResponseDto>, ApiError> {
    let Some(request_id) = request.request_id() else {
        return Ok(None);
    };
    if let Some(existing_turn) = state
        .session_store
        .canonical_turn_for_request_id(&request_id)
    {
        // 一个 Turn 可以接收多条 steer；只能比较对应 request 的输入指纹，
        // 不能拿首条用户消息或 Turn 的指纹拒绝后续补充的重试。
        let stored_fingerprint = existing_turn
            .items
            .iter()
            .find(|item| {
                item.metadata.get("requestId").and_then(Value::as_str) == Some(request_id.as_str())
            })
            .and_then(|item| {
                item.metadata
                    .get("requestFingerprint")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                (existing_turn
                    .metadata
                    .get("requestId")
                    .and_then(Value::as_str)
                    == Some(request_id.as_str()))
                .then(|| {
                    existing_turn
                        .metadata
                        .get("requestFingerprint")
                        .and_then(Value::as_str)
                })
                .flatten()
            });
        if stored_fingerprint != Some(request_fingerprint) {
            return Err(ApiError::Conflict(
                "requestId 已绑定另一份 Turn，请为新请求生成新的 requestId".to_string(),
            ));
        }
        return canonical_turn_replay_response(state, existing_turn, &request_id).map(Some);
    }
    if let Some(input) = state.session_store.input_for_request(&request_id) {
        if input.request_fingerprint != request_fingerprint {
            return Err(ApiError::Conflict(
                "requestId 已绑定另一份输入或排队消息".into(),
            ));
        }
        if input.state == magi_session_store::SessionInputState::Cancelled {
            return Err(ApiError::Conflict(
                "这条输入已撤销，请为新输入生成新的 requestId".into(),
            ));
        }
    }
    if !include_queue {
        return Ok(None);
    }
    // 排队中的请求尚未拥有 canonical Turn，但 requestId 仍然必须保持幂等；
    // 否则网络重试会把同一条用户消息复制到队列，并在队首 drain 时执行两次。
    if let Some((queued, queue_position)) =
        state.queued_regular_session_turn_for_request_id(&request_id)
    {
        let stored_fingerprint = queued
            .request_fingerprint
            .clone()
            .or_else(|| queued.request.request_fingerprint().ok());
        if stored_fingerprint.as_deref() != Some(request_fingerprint) {
            return Err(ApiError::Conflict(
                "requestId 已绑定另一条排队消息，请为新请求生成新的 requestId".to_string(),
            ));
        }
        return queued_turn_replay_response(state, &queued, queue_position).map(Some);
    }
    Ok(None)
}

pub(super) fn canonical_turn_replay_response(
    state: &ApiState,
    canonical_turn: CanonicalTurn,
    request_id: &str,
) -> Result<SessionTurnResponseDto, ApiError> {
    let user_item = canonical_turn
        .items
        .iter()
        .find(|item| {
            item.kind == CanonicalTurnItemKind::UserMessage
                && item.metadata.get("requestId").and_then(Value::as_str) == Some(request_id)
        })
        .cloned();
    let route = user_item
        .as_ref()
        .and_then(|item| item.metadata.get("route").and_then(Value::as_str))
        .or_else(|| canonical_turn.metadata.get("route").and_then(Value::as_str))
        .map(|route| match route {
            "execute" => SessionTurnRouteDto::Execute,
            "continue" => SessionTurnRouteDto::Continue,
            "steer" => SessionTurnRouteDto::Steer,
            _ => SessionTurnRouteDto::Chat,
        })
        .unwrap_or_else(|| {
            if canonical_turn
                .metadata
                .get("executionProfile")
                .and_then(Value::as_str)
                == Some("task")
            {
                SessionTurnRouteDto::Execute
            } else {
                SessionTurnRouteDto::Chat
            }
        });
    let task_id = user_item
        .as_ref()
        .and_then(|item| item.worker.as_ref())
        .and_then(|worker| worker.task_id.clone());
    let event_id = EventId::new(format!(
        "event-session-turn-replay-{}-{}",
        canonical_turn.turn_id, request_id
    ));
    let next_sequence = state.event_bus.snapshot().next_sequence;
    let event_seq = replay_as_of_sequence(next_sequence);
    let steered_turn_id =
        (route == SessionTurnRouteDto::Steer).then(|| canonical_turn.turn_id.clone());
    let accepted_at = user_item
        .as_ref()
        .map(|item| item.created_at)
        .unwrap_or(canonical_turn.accepted_at);
    let response = SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id: canonical_turn.session_id.clone(),
        entry_id: format!("timeline-{}-{}", canonical_turn.session_id, accepted_at.0),
        event_id: event_id.clone(),
        accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: next_sequence,
        created_session: false,
        route,
        root_task_id: task_id.clone(),
        action_task_id: task_id,
        execution_chain_ref: None,
        user_message_item_id: user_item.as_ref().map(|item| item.item_id.clone()),
    })
    .with_canonical_event(
        if steered_turn_id.is_some() {
            "turn_item_upsert"
        } else {
            "turn_started"
        },
        Some(canonical_turn),
        user_item,
    )
    .with_canonical_event_metadata(event_id, event_seq, UtcMillis::now())
    .with_request_identity(Some(request_id.to_string()), event_seq)
    .with_replayed();
    Ok(match steered_turn_id {
        Some(turn_id) => response.with_steered_turn(turn_id),
        None => response,
    })
}

pub(super) fn queued_turn_replay_response(
    state: &ApiState,
    queued: &QueuedRegularSessionTurn,
    queue_position: usize,
) -> Result<SessionTurnResponseDto, ApiError> {
    let event_id = EventId::new(format!(
        "event-session-turn-queued-replay-{}",
        queued.queue_id
    ));
    let next_sequence = state.event_bus.snapshot().next_sequence;
    let event_sequence = replay_as_of_sequence(next_sequence);
    Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id: queued.session_id.clone(),
        entry_id: queued.queue_id.clone(),
        event_id: event_id.clone(),
        accepted_at: queued.accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: next_sequence,
        created_session: false,
        route: queued.route,
        root_task_id: None,
        action_task_id: None,
        execution_chain_ref: None,
        user_message_item_id: queued.request.user_message_id(),
    })
    .with_queued(queued.queue_id.clone(), queue_position)
    .with_request_identity(queued.request.request_id(), event_sequence)
    .with_replayed())
}

pub(super) fn conversation_turn_replay_response(
    state: &ApiState,
    session_id: &SessionId,
    attempt: &magi_conversation_runtime::TurnAttempt,
) -> Result<SessionTurnResponseDto, ApiError> {
    let canonical_turn = state
        .session_store
        .canonical_turn_for_session_turn_id(session_id, &attempt.turn_id)
        .ok_or_else(|| ApiError::Conflict("conversation Turn 重放事实尚未可读".to_string()))?;
    let canonical_item = canonical_turn
        .items
        .iter()
        .find(|item| item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage)
        .cloned();
    let item_id = canonical_item.as_ref().map(|item| item.item_id.clone());
    let event_id = EventId::new(format!(
        "event-session-turn-conversation-{}",
        canonical_turn.turn_id
    ));
    let next_sequence = state.event_bus.snapshot().next_sequence;
    Ok(SessionTurnResponseDto::new(SessionTurnResponseInput {
        session_id: session_id.clone(),
        entry_id: format!("timeline-{}-{}", session_id, canonical_turn.accepted_at.0),
        event_id: event_id.clone(),
        accepted_at: canonical_turn.accepted_at,
        runtime_epoch: state.runtime_epoch().to_string(),
        event_stream_next_sequence: next_sequence,
        created_session: false,
        route: SessionTurnRouteDto::Chat,
        root_task_id: None,
        action_task_id: None,
        execution_chain_ref: None,
        user_message_item_id: item_id,
    })
    .with_canonical_event("turn_started", Some(canonical_turn), canonical_item)
    .with_canonical_event_metadata(
        event_id,
        replay_as_of_sequence(next_sequence),
        UtcMillis::now(),
    )
    .with_replayed())
}

/// 重放响应不对应新发布的事件，它反映的是截至最后一条已发布事件的事实。
/// 标为下一条实时事件的序号会让客户端随后丢弃那条真实事件。
pub(super) fn replay_as_of_sequence(next_sequence: u64) -> u64 {
    next_sequence.saturating_sub(1)
}
