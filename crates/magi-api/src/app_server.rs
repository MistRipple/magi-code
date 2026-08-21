//! 面向 Desktop / Web 宿主的双向 App Server 通道。
//!
//! 该通道只负责协议、生命周期和事件投递。会话、Turn、浏览器和任务的业务权威
//! 仍由现有 ApiState、SessionStore、EventBus 与 BrowserAuthority 持有，避免再造
//! 一份与 HTTP API 并行的状态模型。

use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use magi_app_server_protocol::{
    ClientCapabilities, ClientInfo, ClientMessage, ClientNotification, ClientRequest,
    ERROR_ALREADY_INITIALIZED, ERROR_INTERNAL, ERROR_INVALID_PARAMS, ERROR_INVALID_REQUEST,
    ERROR_METHOD_NOT_FOUND, ERROR_NOT_INITIALIZED, ERROR_REQUEST_CONFLICT, ERROR_SERVER_OVERLOADED,
    ERROR_SESSION_NOT_FOUND, ErrorObject, InitializeParams, InitializeResult, ProtocolVersion,
    RequestId, ServerCapabilities, classify_client_message, error_response, notification, response,
};
use magi_core::{SessionId, WorkspaceId};
use magi_event_bus::{EventEnvelope, EventStreamSnapshot};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast, mpsc};

use crate::{errors::ApiError, routes::sessions, state::ApiState};

const MAX_IN_FLIGHT_REQUESTS: usize = 32;
const CONTROL_QUEUE_CAPACITY: usize = MAX_IN_FLIGHT_REQUESTS + 16;
const EVENT_QUEUE_CAPACITY: usize = 64;

pub fn routes() -> Router<ApiState> {
    Router::new().route("/app-server", get(connect))
}

async fn connect(State(state): State<ApiState>, websocket: WebSocketUpgrade) -> Response {
    websocket.on_upgrade(move |socket| run_connection(socket, state))
}

#[derive(Clone, Debug, Default)]
struct EventSubscription {
    active: bool,
    session_id: Option<SessionId>,
    workspace_id: Option<WorkspaceId>,
    after_sequence: u64,
}

#[derive(Clone, Debug, Default)]
struct ConnectionState {
    initialize_seen: bool,
    initialized: bool,
    client_info: Option<ClientInfo>,
    capabilities: ClientCapabilities,
    subscription: EventSubscription,
}

#[derive(Clone)]
struct OutgoingChannels {
    control: mpsc::Sender<SequencedMessage>,
    events: mpsc::Sender<SequencedMessage>,
    next_sequence: Arc<std::sync::atomic::AtomicU64>,
    sequence_lock: Arc<std::sync::Mutex<()>>,
    disconnected: Arc<AtomicBool>,
    disconnect_notify: Arc<Notify>,
}

struct SequencedMessage {
    sequence: u64,
    message: Message,
}

impl OutgoingChannels {
    fn control_message(&self, message: Message) -> Result<(), mpsc::error::TrySendError<Message>> {
        let _sequence_guard = self
            .sequence_lock
            .lock()
            .expect("App Server 出站序列锁不能中毒");
        let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
        self.control
            .try_send(SequencedMessage { sequence, message })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(message) => {
                    self.next_sequence.fetch_sub(1, Ordering::SeqCst);
                    mpsc::error::TrySendError::Full(message.message)
                }
                mpsc::error::TrySendError::Closed(message) => {
                    self.next_sequence.fetch_sub(1, Ordering::SeqCst);
                    mpsc::error::TrySendError::Closed(message.message)
                }
            })
    }

    fn event_message(&self, message: Message) -> Result<(), mpsc::error::TrySendError<Message>> {
        let _sequence_guard = self
            .sequence_lock
            .lock()
            .expect("App Server 出站序列锁不能中毒");
        let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
        self.events
            .try_send(SequencedMessage { sequence, message })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(message) => {
                    self.next_sequence.fetch_sub(1, Ordering::SeqCst);
                    mpsc::error::TrySendError::Full(message.message)
                }
                mpsc::error::TrySendError::Closed(message) => {
                    self.next_sequence.fetch_sub(1, Ordering::SeqCst);
                    mpsc::error::TrySendError::Closed(message.message)
                }
            })
    }

    fn disconnect(&self) {
        if !self.disconnected.swap(true, Ordering::SeqCst) {
            self.disconnect_notify.notify_one();
        }
    }
}

async fn run_connection(socket: WebSocket, state: ApiState) {
    let (mut sink, mut stream) = socket.split();
    let (control_tx, mut control_rx) = mpsc::channel::<SequencedMessage>(CONTROL_QUEUE_CAPACITY);
    let (event_tx, mut event_rx) = mpsc::channel::<SequencedMessage>(EVENT_QUEUE_CAPACITY);
    let disconnected = Arc::new(AtomicBool::new(false));
    let disconnect_notify = Arc::new(Notify::new());
    let next_sequence = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let sequence_lock = Arc::new(std::sync::Mutex::new(()));
    let outgoing = OutgoingChannels {
        control: control_tx,
        events: event_tx,
        next_sequence,
        sequence_lock,
        disconnected: Arc::clone(&disconnected),
        disconnect_notify: Arc::clone(&disconnect_notify),
    };
    let writer_disconnected = Arc::clone(&disconnected);
    let writer_disconnect_notify = Arc::clone(&disconnect_notify);
    let writer = tokio::spawn(async move {
        let mut control_open = true;
        let mut events_open = true;
        let mut pending = BTreeMap::<u64, Message>::new();
        let mut next_sequence = 1;
        let mut drain_requested = false;
        while control_open || events_open || !pending.is_empty() {
            drain_requested |= writer_disconnected.load(Ordering::SeqCst);
            if let Some(message) = pending.remove(&next_sequence) {
                if sink.send(message).await.is_err() {
                    writer_disconnected.store(true, Ordering::SeqCst);
                    writer_disconnect_notify.notify_one();
                    break;
                }
                next_sequence = next_sequence.saturating_add(1);
                continue;
            }
            tokio::select! {
                biased;
                message = control_rx.recv(), if control_open => {
                    match message {
                        Some(message) => {
                            pending.insert(message.sequence, message.message);
                        }
                        None => control_open = false,
                    }
                }
                message = event_rx.recv(), if events_open => {
                    match message {
                        Some(message) => {
                            pending.insert(message.sequence, message.message);
                        }
                        None => events_open = false,
                    }
                }
                _ = writer_disconnect_notify.notified() => {
                    drain_requested = true;
                },
            }
            if drain_requested && !control_open && !events_open && pending.is_empty() {
                break;
            }
        }
    });

    // 在连接建立瞬间执行 snapshot + subscribe，后续发送 snapshot 时不会丢掉并发事件。
    let (initial_snapshot, mut event_rx) = state.event_bus.snapshot_and_subscribe();
    let connection_state = Arc::new(Mutex::new(ConnectionState::default()));
    let request_slots = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
    let mut request_tasks = tokio::task::JoinSet::new();
    let mut initial_snapshot = Some(initial_snapshot);
    let mut event_subscription_active = false;

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(message)) = incoming else { break };
                match message {
                    Message::Text(text) => {
                        let value = match serde_json::from_str::<Value>(&text) {
                            Ok(value) => value,
                            Err(error) => {
                                let _ = send_protocol_error(
                                    &outgoing,
                                    None,
                                    ERROR_INVALID_REQUEST,
                                    format!("JSON 无效: {error}"),
                                    false,
                                );
                                continue;
                            }
                        };
                        let message = match classify_client_message(&value) {
                            Ok(message) => message,
                            Err(error) => {
                                let _ = send_protocol_error(
                                    &outgoing,
                                    None,
                                    ERROR_INVALID_REQUEST,
                                    error.to_string(),
                                    false,
                                );
                                continue;
                            }
                        };
                        match message {
                            ClientMessage::Request(request) if request.is_initialize() => {
                                let response = initialize_connection(
                                    &state,
                                    &connection_state,
                                    request,
                                ).await;
                                if try_send_json(&outgoing, response).is_err() {
                                    break;
                                }
                            }
                            ClientMessage::Request(request) if request.method == "events/subscribe" => {
                                if initial_snapshot.is_none() {
                                    let (snapshot, replacement) = state.event_bus.snapshot_and_subscribe();
                                    event_rx = replacement;
                                    initial_snapshot = Some(snapshot);
                                }
                                let response = subscribe_events(
                                    &state,
                                    &connection_state,
                                    &mut initial_snapshot,
                                    request,
                                    &outgoing,
                                ).await;
                                event_subscription_active = is_subscribed(&connection_state).await;
                                if try_send_json(&outgoing, response).is_err() {
                                    break;
                                }
                            }
                            ClientMessage::Request(request) => {
                                // 普通请求在进入异步任务前必须完成握手检查，避免
                                // initialized 与业务请求的到达顺序在不同任务间产生竞态。
                                if let Some(error) = require_initialized(&connection_state).await {
                                    if try_send_json(&outgoing, error_response(request.id, error)).is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                let slot = match request_slots.clone().try_acquire_owned() {
                                    Ok(slot) => slot,
                                    Err(_) => {
                                        let _ = try_send_json(
                                            &outgoing,
                                            error_response(
                                                request.id,
                                                ErrorObject::new(
                                                    ERROR_SERVER_OVERLOADED,
                                                    "服务端请求过多，请稍后重试",
                                                ).retryable(true),
                                            ),
                                        );
                                        continue;
                                    }
                                };
                                let state_for_request = state.clone();
                                let connection_for_request = connection_state.clone();
                                let outgoing_for_request = outgoing.clone();
                                request_tasks.spawn(async move {
                                    let _slot = slot;
                                    let response = dispatch_request(
                                        &state_for_request,
                                        &connection_for_request,
                                        request,
                                    ).await;
                                    if try_send_json(&outgoing_for_request, response).is_err() {
                                        outgoing_for_request.disconnect();
                                    }
                                });
                            }
                            ClientMessage::Notification(notification) => {
                                handle_notification(&connection_state, notification).await;
                            }
                            ClientMessage::Response(_) => {
                                let _ = send_protocol_error(
                                    &outgoing,
                                    None,
                                    ERROR_INVALID_REQUEST,
                                    "当前连接没有待处理的服务端请求".to_string(),
                                    false,
                                );
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if outgoing.control_message(Message::Pong(payload)).is_err() {
                            outgoing.disconnect();
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Binary(_) | Message::Pong(_) => {}
                }
            }
            event = event_rx.recv(), if event_subscription_active => {
                match event {
                    Ok(event) => {
                        let should_send = {
                            let guard = connection_state.lock().await;
                            guard.subscription.active
                                && event.sequence > guard.subscription.after_sequence
                                && event_matches(&event, &guard.subscription)
                        };
                        if should_send
                            && !is_notification_opted_out(
                                &connection_state,
                                &format!("event/{}", event.event_type),
                            )
                            .await
                            && try_send_event_json(
                                &outgoing,
                                notification(
                                    format!("event/{}", event.event_type),
                                    json!({"sequence": event.sequence, "event": event}),
                                ),
                            )
                            .is_err()
                        {
                            let _ = try_send_json(
                                &outgoing,
                                notification(
                                    "events/resyncRequired",
                                    json!({"reason": "clientTooSlow"}),
                                ),
                            );
                            outgoing.disconnect();
                            break;
                        }
                        if should_send {
                            connection_state.lock().await.subscription.after_sequence = event.sequence;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let (snapshot, replacement) = state.event_bus.snapshot_and_subscribe();
                        event_rx = replacement;
                        if is_subscribed(&connection_state).await {
                            connection_state.lock().await.subscription.after_sequence =
                                snapshot.next_sequence.saturating_sub(1);
                            if try_send_json(&outgoing, notification(
                                "events/resyncRequired",
                                json!({
                                    "skipped": skipped,
                                    "snapshot": filtered_snapshot(&snapshot, &connection_state).await,
                                }),
                            )).is_err() {
                                outgoing.disconnect();
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = disconnect_notify.notified() => break,
            completed = request_tasks.join_next(), if !request_tasks.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::debug!(?error, "App Server 请求任务结束");
                }
            }
        }
    }

    request_tasks.abort_all();
    while request_tasks.join_next().await.is_some() {}
    drop(outgoing);
    let _ = writer.await;
}

async fn initialize_connection(
    state: &ApiState,
    connection_state: &Arc<Mutex<ConnectionState>>,
    request: ClientRequest,
) -> magi_app_server_protocol::ServerResponse {
    let params = match serde_json::from_value::<InitializeParams>(request.params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request.id,
                ErrorObject::new(
                    ERROR_INVALID_PARAMS,
                    format!("initialize 参数无效: {error}"),
                ),
            );
        }
    };
    let mut guard = connection_state.lock().await;
    if guard.initialize_seen {
        return error_response(
            request.id,
            ErrorObject::new(ERROR_ALREADY_INITIALIZED, "当前连接已经初始化"),
        );
    }
    if let Some(client_protocol) = params.protocol.as_ref()
        && !ProtocolVersion::CURRENT.is_compatible_with(client_protocol)
    {
        return error_response(
            request.id,
            ErrorObject::new(
                ERROR_INVALID_PARAMS,
                format!(
                    "协议版本不兼容: client={}.{} server={}.{}",
                    client_protocol.major,
                    client_protocol.minor,
                    ProtocolVersion::CURRENT.major,
                    ProtocolVersion::CURRENT.minor
                ),
            ),
        );
    }
    if params.client_info.name.trim().is_empty() {
        return error_response(
            request.id,
            ErrorObject::new(ERROR_INVALID_PARAMS, "clientInfo.name 不能为空"),
        );
    }
    guard.initialize_seen = true;
    guard.client_info = Some(params.client_info);
    guard.capabilities = params.capabilities;
    response(
        request.id,
        serde_json::to_value(InitializeResult {
            server_info: ClientInfo {
                name: state.service_info.service_name.clone(),
                title: Some("Magi App Server".to_string()),
                version: Some(state.service_info.api_version.clone()),
            },
            protocol: ProtocolVersion::CURRENT,
            runtime_epoch: state.runtime_epoch().to_string(),
            capabilities: ServerCapabilities {
                sessions: true,
                turns: true,
                events: true,
                approvals: false,
                // 浏览器工具仍由现有 Browser Tool Runtime 负责；App Server 尚未暴露
                // 对应方法时不能提前宣称能力，否则客户端会把请求发到不存在的方法。
                browser_tools: false,
            },
        })
        .expect("初始化响应必须可序列化"),
    )
}

async fn subscribe_events(
    state: &ApiState,
    connection_state: &Arc<Mutex<ConnectionState>>,
    initial_snapshot: &mut Option<EventStreamSnapshot>,
    request: ClientRequest,
    outgoing: &OutgoingChannels,
) -> magi_app_server_protocol::ServerResponse {
    if let Some(error) = require_initialized(connection_state).await {
        return error_response(request.id, error);
    }
    let params = match serde_json::from_value::<EventSubscribeParams>(request.params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request.id,
                ErrorObject::new(
                    ERROR_INVALID_PARAMS,
                    format!("events/subscribe 参数无效: {error}"),
                ),
            );
        }
    };
    let (session_id, workspace_id) = match parse_subscription_scope(&params) {
        Ok(scope) => scope,
        Err(error) => {
            return error_response(request.id, ErrorObject::new(ERROR_INVALID_PARAMS, error));
        }
    };
    let after_sequence = params.after_sequence.unwrap_or(0);
    {
        let mut guard = connection_state.lock().await;
        guard.subscription = EventSubscription {
            active: true,
            session_id,
            workspace_id,
            after_sequence,
        };
    }
    let snapshot = initial_snapshot
        .take()
        .unwrap_or_else(|| state.event_bus.snapshot());
    let resync_required = snapshot_requires_resync(&snapshot, after_sequence);
    let snapshot = filter_snapshot(snapshot, after_sequence, connection_state).await;
    let next_sequence = snapshot.next_sequence;
    // snapshot_and_subscribe 的快照与实时 receiver 是一个连续切面。快照已经
    // 包含截至 nextSequence - 1 的事件，游标必须前移到该边界，否则同一事件
    // 会先出现在 snapshot、随后又从 broadcast receiver 重复投递一次。
    connection_state.lock().await.subscription.after_sequence =
        after_sequence.max(next_sequence.saturating_sub(1));
    let notification_method = if resync_required {
        "events/resyncRequired"
    } else {
        "events/snapshot"
    };
    let mut notification_params = if resync_required {
        json!({"snapshot": snapshot})
    } else {
        serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({}))
    };
    if resync_required {
        notification_params["reason"] = json!("afterSequenceExpired");
        notification_params["requestedAfterSequence"] = json!(after_sequence);
    }
    if try_send_json(
        outgoing,
        notification(notification_method, notification_params),
    )
    .is_err()
    {
        outgoing.disconnect();
    }
    response(
        request.id,
        json!({
            "subscribed": true,
            "nextSequence": next_sequence,
            "resyncRequired": resync_required,
        }),
    )
}

async fn dispatch_request(
    state: &ApiState,
    connection_state: &Arc<Mutex<ConnectionState>>,
    request: ClientRequest,
) -> magi_app_server_protocol::ServerResponse {
    if let Some(error) = require_initialized(connection_state).await {
        return error_response(request.id, error);
    }
    match request.method.as_str() {
        "ping" => response(request.id, json!({"runtimeEpoch": state.runtime_epoch()})),
        "session/list" => list_sessions(state, request.id, request.params).await,
        "session/read" => read_session(state, request.id, request.params).await,
        "turn/start" => start_turn(state, request.id, request.params).await,
        _ => error_response(
            request.id,
            ErrorObject::new(
                ERROR_METHOD_NOT_FOUND,
                format!("未知方法: {}", request.method),
            ),
        ),
    }
}

async fn list_sessions(
    state: &ApiState,
    request_id: RequestId,
    params: Value,
) -> magi_app_server_protocol::ServerResponse {
    let params = match serde_json::from_value::<SessionListParams>(params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(ERROR_INVALID_PARAMS, error.to_string()),
            );
        }
    };
    let sessions = match params
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(workspace_id) => state.session_store.sessions_for_workspace(workspace_id),
        None => state.session_store.sessions(),
    };
    response(
        request_id,
        json!({"sessions": sessions, "runtimeEpoch": state.runtime_epoch()}),
    )
}

async fn read_session(
    state: &ApiState,
    request_id: RequestId,
    params: Value,
) -> magi_app_server_protocol::ServerResponse {
    let params = match serde_json::from_value::<SessionReadParams>(params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(ERROR_INVALID_PARAMS, error.to_string()),
            );
        }
    };
    let session_id = SessionId::new(params.session_id.trim());
    let Some(session) = state.session_store.session(&session_id) else {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_SESSION_NOT_FOUND, "会话不存在"),
        );
    };
    let turns = if params.include_turns {
        serde_json::to_value(state.session_store.canonical_turns_for_session(&session_id))
            .unwrap_or_else(|_| json!([]))
    } else {
        json!([])
    };
    response(
        request_id,
        json!({"session": session, "turns": turns, "runtimeEpoch": state.runtime_epoch()}),
    )
}

async fn start_turn(
    state: &ApiState,
    request_id: RequestId,
    params: Value,
) -> magi_app_server_protocol::ServerResponse {
    let request = match serde_json::from_value::<crate::dto::SessionTurnRequestDto>(params) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(ERROR_INVALID_PARAMS, error.to_string()),
            );
        }
    };
    let business_request_id = request.request_id();
    if let Some(business_request_id) = business_request_id.as_deref() {
        let _request_lock = state.lock_app_server_request(business_request_id).await;
        return start_turn_once(state, request_id, request).await;
    }
    start_turn_once(state, request_id, request).await
}

async fn start_turn_once(
    state: &ApiState,
    request_id: RequestId,
    request: crate::dto::SessionTurnRequestDto,
) -> magi_app_server_protocol::ServerResponse {
    if let Some(existing_turn) = request.request_id().as_deref().and_then(|request_id| {
        state
            .session_store
            .canonical_turn_for_request_id(request_id)
    }) {
        if !request_matches_canonical_turn(&request, &existing_turn) {
            return error_response(
                request_id,
                ErrorObject::new(
                    ERROR_REQUEST_CONFLICT,
                    "requestId 已绑定另一份 Turn，请为新请求生成新的 requestId",
                )
                .retryable(false),
            );
        }
        let user_message_item = existing_turn
            .items
            .iter()
            .find(|item| item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage);
        response(
            request_id,
            json!({
                    "replayed": true,
                    "entryId": format!("timeline-{}-{}", existing_turn.session_id, existing_turn.accepted_at.0),
                    "eventId": format!("event-session-turn-task-{}", existing_turn.accepted_at.0),
                    "sessionId": existing_turn.session_id,
                    "turnId": existing_turn.turn_id,
                    "acceptedAt": existing_turn.accepted_at,
                    "createdSession": false,
                    "route": user_message_item.and_then(|item| {
                        item.metadata.get("route").and_then(Value::as_str)
                    }),
                    "queued": false,
                    "userMessageItemId": user_message_item.map(|item| item.item_id.clone()),
                    "runtimeEpoch": state.runtime_epoch(),
                    "eventStreamNextSequence": state.event_bus.snapshot().next_sequence,
                    "canonicalTurn": existing_turn,
            }),
        )
    } else if let Some((queued_turn, queue_position)) = request
        .request_id()
        .as_deref()
        .and_then(|request_id| state.queued_regular_session_turn_for_request_id(request_id))
    {
        if !request_matches_queued_turn(&request, &queued_turn) {
            return error_response(
                request_id,
                ErrorObject::new(
                    ERROR_REQUEST_CONFLICT,
                    "requestId 已绑定另一份排队 Turn，请为新请求生成新的 requestId",
                )
                .retryable(false),
            );
        }
        let queue_id = queued_turn.queue_id.clone();
        let user_message_item_id = queued_turn.request.user_message_id();
        response(
            request_id,
            json!({
                    "replayed": true,
                    "entryId": queue_id,
                    "eventId": format!("event-session-turn-queued-{}", queued_turn.accepted_at.0),
                    "queued": true,
                    "queueId": queued_turn.queue_id,
                    "queuePosition": queue_position,
                    "sessionId": queued_turn.session_id,
                    "workspaceId": queued_turn.workspace_id,
                    "acceptedAt": queued_turn.accepted_at,
                    "route": queued_turn.route,
                    "createdSession": false,
                    "userMessageItemId": user_message_item_id,
                "runtimeEpoch": state.runtime_epoch(),
                "eventStreamNextSequence": state.event_bus.snapshot().next_sequence,
            }),
        )
    } else {
        let business_request_id = request.request_id();
        match sessions::submit_session_turn(
            axum::extract::State(state.clone()),
            axum::Json(request),
        )
        .await
        {
            Ok(axum::Json(result)) => {
                let mut result = serde_json::to_value(result).unwrap_or_else(|_| json!({}));
                result["replayed"] = json!(false);
                if let Some(business_request_id) = business_request_id {
                    result["requestId"] = json!(business_request_id);
                }
                response(request_id, result)
            }
            Err(error) => error_response(request_id, api_error_to_protocol(error)),
        }
    }
}

fn request_matches_canonical_turn(
    request: &crate::dto::SessionTurnRequestDto,
    turn: &magi_session_store::CanonicalTurn,
) -> bool {
    let Some(user_item) = turn
        .items
        .iter()
        .find(|item| item.kind == magi_session_store::CanonicalTurnItemKind::UserMessage)
    else {
        return false;
    };
    let Some(request_fingerprint) = request.request_fingerprint().ok() else {
        return false;
    };
    user_item
        .metadata
        .get("requestFingerprint")
        .and_then(Value::as_str)
        == Some(request_fingerprint.as_str())
}

fn request_matches_queued_turn(
    request: &crate::dto::SessionTurnRequestDto,
    queued: &crate::state::QueuedRegularSessionTurn,
) -> bool {
    let Some(request_fingerprint) = request.request_fingerprint().ok() else {
        return false;
    };
    queued.request_fingerprint.as_deref() == Some(request_fingerprint.as_str())
}

async fn handle_notification(
    connection_state: &Arc<Mutex<ConnectionState>>,
    message: ClientNotification,
) {
    if message.method == "initialized" {
        let mut guard = connection_state.lock().await;
        if guard.initialize_seen {
            guard.initialized = true;
        }
    }
}

async fn require_initialized(
    connection_state: &Arc<Mutex<ConnectionState>>,
) -> Option<ErrorObject> {
    (!connection_state.lock().await.initialized).then(|| {
        ErrorObject::new(
            ERROR_NOT_INITIALIZED,
            "连接尚未完成 initialize/initialized 握手",
        )
    })
}

async fn is_subscribed(connection_state: &Arc<Mutex<ConnectionState>>) -> bool {
    connection_state.lock().await.subscription.active
}

async fn is_notification_opted_out(
    connection_state: &Arc<Mutex<ConnectionState>>,
    method: &str,
) -> bool {
    connection_state.lock().await.capabilities.opted_out(method)
}

async fn filtered_snapshot(
    snapshot: &EventStreamSnapshot,
    connection_state: &Arc<Mutex<ConnectionState>>,
) -> EventStreamSnapshot {
    let subscription = connection_state.lock().await.subscription.clone();
    EventStreamSnapshot {
        next_sequence: snapshot.next_sequence,
        recent_events: snapshot
            .recent_events
            .iter()
            .filter(|event| event_matches(event, &subscription))
            .cloned()
            .collect(),
    }
}

async fn filter_snapshot(
    snapshot: EventStreamSnapshot,
    after_sequence: u64,
    connection_state: &Arc<Mutex<ConnectionState>>,
) -> EventStreamSnapshot {
    let snapshot = filtered_snapshot(&snapshot, connection_state).await;
    EventStreamSnapshot {
        next_sequence: snapshot.next_sequence,
        recent_events: snapshot
            .recent_events
            .into_iter()
            .filter(|event| event.sequence > after_sequence)
            .collect(),
    }
}

fn snapshot_requires_resync(snapshot: &EventStreamSnapshot, after_sequence: u64) -> bool {
    if after_sequence == 0 {
        return false;
    }
    let expected_next = after_sequence.saturating_add(1);
    snapshot
        .recent_events
        .first()
        .map(|event| event.sequence > expected_next)
        .unwrap_or(snapshot.next_sequence > expected_next)
}

fn event_matches(event: &EventEnvelope, subscription: &EventSubscription) -> bool {
    if let Some(session_id) = subscription.session_id.as_ref()
        && event.session_id.as_ref() != Some(session_id)
    {
        return false;
    }
    if let Some(workspace_id) = subscription.workspace_id.as_ref()
        && event.workspace_id.as_ref() != Some(workspace_id)
    {
        return false;
    }
    true
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventSubscribeParams {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    after_sequence: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionListParams {
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionReadParams {
    session_id: String,
    #[serde(default)]
    include_turns: bool,
}

fn parse_subscription_scope(
    params: &EventSubscribeParams,
) -> Result<(Option<SessionId>, Option<WorkspaceId>), String> {
    let session_id = params
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(SessionId::new);
    let workspace_id = params
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(WorkspaceId::new);
    if session_id.is_none() && workspace_id.is_none() {
        return Err("events/subscribe 至少需要 sessionId 或 workspaceId".to_string());
    }
    Ok((session_id, workspace_id))
}

fn api_error_to_protocol(error: ApiError) -> ErrorObject {
    match error {
        ApiError::InvalidInput(message) => ErrorObject::new(ERROR_INVALID_PARAMS, message),
        ApiError::InvalidRequestBody(message) => ErrorObject::new(ERROR_INVALID_PARAMS, message),
        ApiError::SessionNotFound(message) | ApiError::NotFound(message) => {
            ErrorObject::new(ERROR_SESSION_NOT_FOUND, message)
        }
        ApiError::RecoveryNotFound(message) => ErrorObject::new(ERROR_SESSION_NOT_FOUND, message),
        ApiError::Conflict(message) | ApiError::TurnConflict { message, .. } => {
            ErrorObject::new(ERROR_REQUEST_CONFLICT, message).retryable(true)
        }
        ApiError::CapabilityUnavailable { message, .. } => {
            ErrorObject::new(-32011, message).retryable(false)
        }
        ApiError::ModelInvocationFailed(message) | ApiError::InternalAssemblyError(message) => {
            ErrorObject::new(ERROR_INTERNAL, message).retryable(true)
        }
    }
}

fn try_send_json<T: serde::Serialize>(
    outgoing: &OutgoingChannels,
    value: T,
) -> Result<(), mpsc::error::TrySendError<Message>> {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| {
        serde_json::to_string(&error_response(
            RequestId::new("serialization-error").expect("固定 request id 合法"),
            ErrorObject::new(ERROR_INTERNAL, "响应序列化失败"),
        ))
        .expect("内部错误响应必须可序列化")
    });
    outgoing.control_message(Message::Text(text.into()))
}

fn try_send_event_json<T: serde::Serialize>(
    outgoing: &OutgoingChannels,
    value: T,
) -> Result<(), mpsc::error::TrySendError<Message>> {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| {
        serde_json::to_string(&error_response(
            RequestId::new("serialization-error").expect("固定 request id 合法"),
            ErrorObject::new(ERROR_INTERNAL, "响应序列化失败"),
        ))
        .expect("内部错误响应必须可序列化")
    });
    outgoing.event_message(Message::Text(text.into()))
}

fn send_protocol_error(
    outgoing: &OutgoingChannels,
    request_id: Option<RequestId>,
    code: i32,
    message: String,
    retryable: bool,
) -> Result<(), mpsc::error::TrySendError<Message>> {
    let id = request_id
        .unwrap_or_else(|| RequestId::new("protocol-error").expect("固定 request id 合法"));
    try_send_json(
        outgoing,
        error_response(id, ErrorObject::new(code, message).retryable(retryable)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use magi_core::{EventId, TaskCompletionContract, TaskTier, ThreadId, UtcMillis};
    use magi_event_bus::EventCategory;
    use magi_governance::GovernanceService;
    use magi_session_store::{
        CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind, CanonicalTurnItemStatus,
        CanonicalTurnStatus, CanonicalTurnVisibility, SessionStore,
    };
    use magi_workspace::WorkspaceStore;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};

    fn event(sequence: u64, session_id: Option<&str>, workspace_id: Option<&str>) -> EventEnvelope {
        EventEnvelope {
            event_id: EventId::new(format!("event-{sequence}")),
            event_type: "session.changed".to_string(),
            category: EventCategory::Domain,
            occurred_at: UtcMillis(sequence),
            sequence,
            workspace_id: workspace_id.map(WorkspaceId::new),
            session_id: session_id.map(SessionId::new),
            mission_id: None,
            assignment_id: None,
            task_id: None,
            payload: json!({"sequence": sequence}),
        }
    }

    #[test]
    fn event_subscription_requires_at_least_one_scope() {
        let error = parse_subscription_scope(&EventSubscribeParams {
            session_id: None,
            workspace_id: None,
            after_sequence: None,
        })
        .expect_err("未绑定会话或工作区不能订阅全局事件");
        assert!(error.contains("至少需要"));
    }

    #[test]
    fn event_filter_keeps_only_the_requested_scope() {
        let subscription = EventSubscription {
            active: true,
            session_id: Some(SessionId::new("session-1")),
            workspace_id: Some(WorkspaceId::new("workspace-1")),
            after_sequence: 0,
        };
        assert!(event_matches(
            &event(1, Some("session-1"), Some("workspace-1")),
            &subscription
        ));
        assert!(!event_matches(
            &event(2, Some("session-2"), Some("workspace-1")),
            &subscription
        ));
        assert!(!event_matches(
            &event(3, Some("session-1"), Some("workspace-2")),
            &subscription
        ));
    }

    #[test]
    fn expired_event_cursor_requires_resync() {
        let snapshot = EventStreamSnapshot {
            next_sequence: 11,
            recent_events: vec![event(5, Some("session-1"), Some("workspace-1"))],
        };
        assert!(snapshot_requires_resync(&snapshot, 3));
        assert!(!snapshot_requires_resync(&snapshot, 4));
        assert!(!snapshot_requires_resync(&snapshot, 0));
    }

    #[test]
    fn queued_turn_replay_requires_the_persisted_complete_request_fingerprint() {
        let request: crate::dto::SessionTurnRequestDto = serde_json::from_value(json!({
            "scope": "personal",
            "text": "inspect browser",
            "requestId": "request-queued-fingerprint"
        }))
        .expect("request should deserialize");
        let fingerprint = request
            .request_fingerprint()
            .expect("request fingerprint should generate");
        let queued = crate::state::QueuedRegularSessionTurn {
            request: request.clone(),
            request_fingerprint: Some(fingerprint),
            requested_workspace_id: None,
            accepted_at: UtcMillis(1),
            route: crate::dto::SessionTurnRouteDto::Chat,
            task_title: None,
            execution_goal: None,
            task_tier: TaskTier::ExecutionChain,
            tool_intent: None,
            forced_tool_name: None,
            goal_mode: false,
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            session_id: SessionId::new("session-queued-fingerprint"),
            workspace_id: None,
            queue_id: "queue-fingerprint".to_string(),
            retry_count: 0,
        };
        assert!(request_matches_queued_turn(&request, &queued));

        let mut changed = request.clone();
        changed.locale = Some("en-US".to_string());
        assert!(!request_matches_queued_turn(&changed, &queued));

        let mut legacy = queued;
        legacy.request_fingerprint = None;
        assert!(!request_matches_queued_turn(&request, &legacy));
    }

    #[test]
    fn canonical_turn_replay_requires_the_same_complete_request_fingerprint() {
        let request: crate::dto::SessionTurnRequestDto = serde_json::from_value(json!({
            "scope": "personal",
            "text": "inspect browser",
            "requestId": "request-canonical-fingerprint"
        }))
        .expect("request should deserialize");
        let fingerprint = request
            .request_fingerprint()
            .expect("request fingerprint should generate");
        let session_id = SessionId::new("session-canonical-fingerprint");
        let turn = CanonicalTurn {
            session_id: session_id.clone(),
            turn_id: "turn-canonical-fingerprint".to_string(),
            turn_seq: 1,
            accepted_at: UtcMillis(1),
            completed_at: None,
            status: CanonicalTurnStatus::Running,
            response_duration_ms: None,
            usage: None,
            items: vec![CanonicalTurnItem {
                session_id,
                turn_id: "turn-canonical-fingerprint".to_string(),
                turn_seq: 1,
                item_id: "user-canonical-fingerprint".to_string(),
                item_seq: 0,
                kind: CanonicalTurnItemKind::UserMessage,
                created_at: UtcMillis(1),
                status: CanonicalTurnItemStatus::Completed,
                item_version: None,
                updated_at: UtcMillis(1),
                title: None,
                content: Some("inspect browser".to_string()),
                blocks: Vec::new(),
                tool: None,
                worker: None,
                source_thread_id: ThreadId::new("thread-canonical-fingerprint"),
                visibility: CanonicalTurnVisibility::default(),
                metadata: HashMap::from([("requestFingerprint".to_string(), json!(fingerprint))]),
            }],
            metadata: HashMap::new(),
        };
        assert!(request_matches_canonical_turn(&request, &turn));

        let mut changed = request.clone();
        changed.access_profile = Some(magi_core::AccessProfile::ReadOnly);
        assert!(!request_matches_canonical_turn(&changed, &turn));

        let mut legacy = turn;
        legacy.items[0].metadata.remove("requestFingerprint");
        assert!(!request_matches_canonical_turn(&request, &legacy));
    }

    #[tokio::test]
    async fn app_server_request_lock_serializes_same_business_request_id() {
        let state = ApiState::new(
            "magi-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(8)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let first = state.lock_app_server_request("same-request").await;
        let state_for_waiter = state.clone();
        let ready = Arc::new(Notify::new());
        let ready_for_waiter = Arc::clone(&ready);
        let waiter = tokio::spawn(async move {
            let _second = state_for_waiter
                .lock_app_server_request("same-request")
                .await;
            ready_for_waiter.notify_one();
        });
        assert!(
            timeout(Duration::from_millis(50), ready.notified())
                .await
                .is_err()
        );
        drop(first);
        timeout(Duration::from_secs(1), ready.notified())
            .await
            .expect("同一 requestId 的等待者应被释放");
        timeout(Duration::from_secs(1), waiter)
            .await
            .expect("同一 requestId 的等待者应被释放")
            .expect("等待任务不应失败");
    }

    async fn next_text_message(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        loop {
            let message = timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("WebSocket 响应不能超时")
                .expect("WebSocket 应保持连接")
                .expect("WebSocket 帧应有效");
            if let ClientMessage::Text(text) = message {
                return serde_json::from_str(&text).expect("文本帧应为 JSON");
            }
        }
    }

    async fn send_request(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        request: Value,
        request_id: &str,
    ) -> Value {
        socket
            .send(ClientMessage::Text(request.to_string().into()))
            .await
            .expect("请求应发送");
        loop {
            let response = next_text_message(socket).await;
            if response.get("id").and_then(Value::as_str) == Some(request_id)
                || response
                    .get("id")
                    .and_then(Value::as_i64)
                    .map(|id| id.to_string() == request_id)
                    .unwrap_or(false)
            {
                return response;
            }
        }
    }

    #[tokio::test]
    async fn websocket_route_completes_handshake_subscription_and_control_ping() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let session_store = Arc::new(SessionStore::default());
        let session_id = SessionId::new("session-app-server-read");
        session_store
            .create_session_for_workspace_at(
                session_id.clone(),
                "App Server integration",
                Some("workspace-test".to_string()),
                UtcMillis(1),
            )
            .expect("集成测试会话应创建");
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let event_state = state.clone();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("测试监听器应绑定");
        let address = listener.local_addr().expect("应读取测试端口");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::routes::build_router(state))
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("测试路由应正常退出");
        });

        let (mut socket, _) = connect_async(format!("ws://{address}/api/app-server"))
            .await
            .expect("应成功连接 App Server WebSocket");
        let before_initialized = send_request(
            &mut socket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "before-initialized",
                "method": "session/list",
                "params": {}
            }),
            "before-initialized",
        )
        .await;
        assert_eq!(before_initialized["jsonrpc"], "2.0");
        assert_eq!(before_initialized["error"]["code"], ERROR_NOT_INITIALIZED);

        let initialize = send_request(
            &mut socket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "integration-test"},
                    "protocol": {"major": 1, "minor": 0},
                    "capabilities": {"streaming": true}
                }
            }),
            "1",
        )
        .await;
        assert_eq!(initialize["jsonrpc"], "2.0");
        assert_eq!(initialize["id"], 1);
        assert_eq!(initialize["result"]["capabilities"]["events"], true);
        assert_eq!(initialize["result"]["capabilities"]["browserTools"], false);

        socket
            .send(ClientMessage::Text(
                serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("initialized 通知应发送");

        let subscribe = send_request(
            &mut socket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "subscribe-1",
                "method": "events/subscribe",
                "params": {"workspaceId": "workspace-test", "afterSequence": 0}
            }),
            "subscribe-1",
        )
        .await;
        assert_eq!(subscribe["result"]["subscribed"], true);

        let listed = send_request(
            &mut socket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/list",
                "params": {"workspaceId": "workspace-test"}
            }),
            "2",
        )
        .await;
        assert_eq!(listed["id"], 2);
        assert_eq!(
            listed["result"]["sessions"].as_array().map(Vec::len),
            Some(1)
        );

        let read = send_request(
            &mut socket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "read-1",
                "method": "session/read",
                "params": {"sessionId": session_id, "includeTurns": true}
            }),
            "read-1",
        )
        .await;
        assert_eq!(read["result"]["session"]["title"], "App Server integration");
        assert_eq!(read["result"]["turns"].as_array().map(Vec::len), Some(0));

        event_state.event_bus.publish(event(
            1,
            Some("session-app-server-read"),
            Some("workspace-test"),
        ));
        let event_notification = next_text_message(&mut socket).await;
        assert_eq!(event_notification["method"], "event/session.changed");
        assert_eq!(event_notification["params"]["event"]["sequence"], 1);

        socket
            .send(ClientMessage::Ping(vec![1, 2, 3].into()))
            .await
            .expect("WebSocket Ping 应发送");
        let pong = timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("Pong 不能超时")
            .expect("连接应保持")
            .expect("Pong 帧应有效");
        assert_eq!(pong, ClientMessage::Pong(vec![1, 2, 3].into()));

        drop(socket);
        shutdown_tx.send(()).expect("测试服务应收到停止信号");
        server.await.expect("测试服务任务应正常结束");
    }
}
