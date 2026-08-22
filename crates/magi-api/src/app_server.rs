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
    AppServerRequestMethod, ApprovalDecision, ApprovalRequestParams, BrowserAccessProfile,
    BrowserToolParams, BrowserToolsListParams, BrowserToolsListResult, CancelRequestParams,
    ClientCapabilities, ClientInfo, ClientMessage, ClientNotification, ClientRequest,
    ClientResponse, ERROR_ALREADY_INITIALIZED, ERROR_INTERNAL, ERROR_INVALID_PARAMS,
    ERROR_INVALID_REQUEST, ERROR_METHOD_NOT_FOUND, ERROR_NOT_INITIALIZED, ERROR_REQUEST_CANCELLED,
    ERROR_REQUEST_CONFLICT, ERROR_REQUEST_TIMEOUT, ERROR_SERVER_OVERLOADED,
    ERROR_SESSION_NOT_FOUND, ErrorObject, EventNotificationParams, EventResyncRequiredParams,
    EventSubscribeParams, EventSubscribeResult, InitializeParams, InitializeResult, PingResult,
    ProtocolVersion, RequestId, ServerCapabilities, ServerNotification, ServerRequest,
    ServerResponse, SessionListParams, SessionListResult, SessionReadParams, SessionReadResult,
    SessionSummary, TurnStartParams, TurnStartResult, classify_client_message, error_response,
    typed_notification, typed_response,
};
use magi_browser_authority::{BrowserToolAccess, BrowserToolKind};
use magi_core::{
    AccessProfile, EventId, ExecutionResultStatus, SessionId, ThreadId, ToolCallId, UtcMillis,
    WorkspaceId,
};
use magi_event_bus::{EventContext, EventEnvelope, EventStreamSnapshot};
use magi_session_store::ActiveExecutionTurnItem;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicU8;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex, Notify, Semaphore, broadcast, mpsc, oneshot};

use crate::{dto::SessionDirectoryEntryDto, errors::ApiError, routes::sessions, state::ApiState};

const MAX_IN_FLIGHT_REQUESTS: usize = 32;
const CONTROL_QUEUE_CAPACITY: usize = MAX_IN_FLIGHT_REQUESTS + 16;
const EVENT_QUEUE_CAPACITY: usize = 64;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const MIN_REQUEST_TIMEOUT_MS: u64 = 100;
const MAX_REQUEST_TIMEOUT_MS: u64 = 600_000;

fn typed_success<T: Serialize>(request_id: RequestId, result: T) -> ServerResponse {
    match typed_response(request_id.clone(), result) {
        Ok(response) => response,
        Err(error) => error_response(request_id, error),
    }
}

fn typed_success_from_json<T>(request_id: RequestId, value: Value, method: &str) -> ServerResponse
where
    T: DeserializeOwned + Serialize,
{
    match serde_json::from_value::<T>(value) {
        Ok(result) => typed_success(request_id, result),
        Err(error) => error_response(
            request_id,
            ErrorObject::new(
                ERROR_INTERNAL,
                format!("{method} 结果不符合协议 Schema: {error}"),
            ),
        ),
    }
}

fn typed_notification_from_json<T>(
    method: impl Into<String>,
    value: Value,
) -> Result<ServerNotification, ErrorObject>
where
    T: DeserializeOwned + Serialize,
{
    let params = serde_json::from_value::<T>(value).map_err(|error| {
        ErrorObject::new(
            ERROR_INTERNAL,
            format!("通知参数不符合协议 Schema: {error}"),
        )
    })?;
    typed_notification(method, params)
}

#[derive(Clone)]
struct RequestControl {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
    termination: Arc<AtomicU8>,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestTermination {
    None = 0,
    Cancelled = 1,
    TimedOut = 2,
}

impl RequestControl {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            termination: Arc::new(AtomicU8::new(RequestTermination::None as u8)),
        }
    }

    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.termination
                .store(RequestTermination::Cancelled as u8, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    fn timeout(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.termination
                .store(RequestTermination::TimedOut as u8, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    fn termination(&self) -> RequestTermination {
        match self.termination.load(Ordering::SeqCst) {
            value if value == RequestTermination::Cancelled as u8 => RequestTermination::Cancelled,
            value if value == RequestTermination::TimedOut as u8 => RequestTermination::TimedOut,
            _ => RequestTermination::None,
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

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
    let pending_requests = Arc::new(Mutex::new(HashMap::<RequestId, RequestControl>::new()));
    let pending_server_requests = Arc::new(Mutex::new(HashMap::<
        RequestId,
        oneshot::Sender<ClientResponse>,
    >::new()));
    let next_server_request_id = Arc::new(std::sync::atomic::AtomicU64::new(1));
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
                                let pending_requests_for_task = pending_requests.clone();
                                let pending_server_requests_for_task = pending_server_requests.clone();
                                let next_server_request_id_for_task = next_server_request_id.clone();
                                let request_timeout = match request_timeout(&request) {
                                    Ok(timeout) => timeout,
                                    Err(error) => {
                                        let _ = try_send_json(
                                            &outgoing,
                                            error_response(request.id, error),
                                        );
                                        continue;
                                    }
                                };
                                let control = RequestControl::new();
                                pending_requests
                                    .lock()
                                    .await
                                    .insert(request.id.clone(), control.clone());
                                let request_id_for_cleanup = request.id.clone();
                                let side_effecting = matches!(
                                    request.method.as_str(),
                                    "browser/tool" | "approval/request"
                                );
                                let browser_tool_request = request.method == "browser/tool";
                                let browser_timeout_retryable = browser_tool_retryable(&request);
                                request_tasks.spawn(async move {
                                    let _slot = slot;
                                    let request_id = request.id.clone();
                                    let control_for_dispatch = control.clone();
                                    let outgoing_for_dispatch = outgoing_for_request.clone();
                                    let dispatch_task = tokio::spawn(async move {
                                        dispatch_request(
                                            &state_for_request,
                                            &connection_for_request,
                                            &outgoing_for_dispatch,
                                            &pending_server_requests_for_task,
                                            &next_server_request_id_for_task,
                                            request,
                                            control_for_dispatch,
                                            request_timeout,
                                        )
                                        .await
                                    });
                                    let mut dispatch_task = dispatch_task;
                                    let response = if control.is_cancelled() {
                                        if !side_effecting {
                                            dispatch_task.abort();
                                        }
                                        error_response(
                                            request_id,
                                            ErrorObject::new(ERROR_REQUEST_CANCELLED, "请求已取消")
                                                .retryable(false),
                                        )
                                    } else {
                                        tokio::select! {
                                            biased;
                                            _ = control.notify.notified() => {
                                                if !side_effecting {
                                                    dispatch_task.abort();
                                                }
                                                error_response(
                                                    request_id.clone(),
                                                    ErrorObject::new(ERROR_REQUEST_CANCELLED, "请求已取消")
                                                        .retryable(false),
                                                )
                                            }
                                            _ = tokio::time::sleep(request_timeout) => {
                                                control.timeout();
                                                if !side_effecting {
                                                    dispatch_task.abort();
                                                }
                                                let error = if browser_tool_request {
                                                    browser_timeout_error(browser_timeout_retryable)
                                                } else {
                                                    ErrorObject::new(ERROR_REQUEST_TIMEOUT, "请求执行超时")
                                                        .retryable(true)
                                                };
                                                error_response(request_id.clone(), error)
                                            }
                                            result = &mut dispatch_task => {
                                                match result {
                                                    Ok(response) => response,
                                                    Err(error) => error_response(
                                                        request_id,
                                                        ErrorObject::new(
                                                            ERROR_INTERNAL,
                                                            format!("请求任务异常结束: {error}"),
                                                        ),
                                                    ),
                                                }
                                            }
                                        }
                                    };
                                    pending_requests_for_task
                                        .lock()
                                        .await
                                        .remove(&request_id_for_cleanup);
                                    if try_send_json(&outgoing_for_request, response).is_err() {
                                        outgoing_for_request.disconnect();
                                    }
                                });
                            }
                            ClientMessage::Notification(notification) => {
                                if notification.method == "$/cancelRequest" {
                                    handle_cancel_notification(&pending_requests, notification).await;
                                } else {
                                    handle_notification(&connection_state, notification).await;
                                }
                            }
                            ClientMessage::Response(response) => {
                                if !resolve_server_response(&pending_server_requests, response).await {
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
                        {
                            let notification = match typed_notification_from_json::<EventNotificationParams>(
                                format!("event/{}", event.event_type),
                                json!({"sequence": event.sequence, "event": event}),
                            ) {
                                Ok(notification) => notification,
                                Err(error) => {
                                    tracing::error!(%error.message, "事件通知不符合协议 Schema");
                                    outgoing.disconnect();
                                    break;
                                }
                            };
                            if try_send_event_json(&outgoing, notification).is_err() {
                                let resync = typed_notification_from_json::<EventResyncRequiredParams>(
                                    "events/resyncRequired",
                                    json!({
                                        "reason": "clientTooSlow",
                                        "requestedAfterSequence": null,
                                        "skipped": null,
                                        "snapshot": {
                                            "next_sequence": state.event_bus.snapshot().next_sequence,
                                            "recent_events": [],
                                        },
                                    }),
                                );
                                if let Ok(resync) = resync {
                                    let _ = try_send_json(&outgoing, resync);
                                }
                                outgoing.disconnect();
                                break;
                            }
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
                            let snapshot = filtered_snapshot(&snapshot, &connection_state).await;
                            let notification = typed_notification_from_json::<EventResyncRequiredParams>(
                                "events/resyncRequired",
                                json!({
                                    "reason": "clientTooSlow",
                                    "requestedAfterSequence": null,
                                    "skipped": skipped,
                                    "snapshot": snapshot,
                                }),
                            );
                            match notification {
                                Ok(notification) => {
                                    if try_send_json(&outgoing, notification).is_err() {
                                        outgoing.disconnect();
                                        break;
                                    }
                                }
                                Err(_) => {
                                    outgoing.disconnect();
                                    break;
                                }
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

    for control in pending_requests.lock().await.values() {
        control.cancel();
    }
    let drain = async {
        while let Some(result) = request_tasks.join_next().await {
            if let Err(error) = result {
                tracing::debug!(?error, "App Server 请求任务结束");
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(30), drain)
        .await
        .is_err()
    {
        request_tasks.abort_all();
        while request_tasks.join_next().await.is_some() {}
    }
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
    typed_success(
        request.id,
        InitializeResult {
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
                approvals: true,
                // App Server 已提供 browser/tools/list 与 browser/tool。实际是否可执行
                // 由 browser capability snapshot 在请求边界再次校验，而不是通过握手
                // 静态伪装成“没有浏览器能力”。
                browser_tools: true,
            },
        },
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
    let notification = if resync_required {
        typed_notification_from_json::<EventResyncRequiredParams>(
            "events/resyncRequired",
            json!({
                "reason": "afterSequenceExpired",
                "requestedAfterSequence": after_sequence,
                "skipped": null,
                "snapshot": snapshot,
            }),
        )
    } else {
        serde_json::to_value(snapshot)
            .map_err(|error| {
                ErrorObject::new(ERROR_INTERNAL, format!("事件快照序列化失败: {error}"))
            })
            .and_then(|value| {
                typed_notification_from_json::<magi_app_server_protocol::EventStreamSnapshot>(
                    "events/snapshot",
                    value,
                )
            })
    };
    match notification {
        Ok(notification) => {
            if try_send_json(outgoing, notification).is_err() {
                outgoing.disconnect();
            }
        }
        Err(_) => outgoing.disconnect(),
    }
    typed_success(
        request.id,
        EventSubscribeResult {
            subscribed: true,
            next_sequence,
            resync_required,
        },
    )
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_request(
    state: &ApiState,
    connection_state: &Arc<Mutex<ConnectionState>>,
    outgoing: &OutgoingChannels,
    pending_server_requests: &Arc<Mutex<HashMap<RequestId, oneshot::Sender<ClientResponse>>>>,
    next_server_request_id: &Arc<std::sync::atomic::AtomicU64>,
    request: ClientRequest,
    control: RequestControl,
    request_timeout: std::time::Duration,
) -> magi_app_server_protocol::ServerResponse {
    if let Some(error) = require_initialized(connection_state).await {
        return error_response(request.id, error);
    }
    if control.is_cancelled() {
        return error_response(
            request.id,
            ErrorObject::new(ERROR_REQUEST_CANCELLED, "请求已取消").retryable(false),
        );
    }
    let method = AppServerRequestMethod::parse(&request.method);
    match method {
        Some(AppServerRequestMethod::Ping) => typed_success(
            request.id,
            PingResult {
                runtime_epoch: state.runtime_epoch().to_string(),
            },
        ),
        Some(AppServerRequestMethod::SessionList) => {
            list_sessions(state, request.id, request.params).await
        }
        Some(AppServerRequestMethod::SessionRead) => {
            read_session(state, request.id, request.params).await
        }
        Some(AppServerRequestMethod::TurnStart) => {
            start_turn(state, request.id, request.params).await
        }
        Some(AppServerRequestMethod::BrowserToolsList) => {
            list_browser_tools(state, request.id, request.params).await
        }
        Some(AppServerRequestMethod::BrowserTool) => {
            execute_browser_tool(state, request.id, request.params, control).await
        }
        Some(AppServerRequestMethod::ApprovalRequest) => {
            request_approval(
                ApprovalRequestContext {
                    state,
                    connection_state,
                    outgoing,
                    pending_server_requests,
                    next_server_request_id,
                    control,
                },
                request.id,
                request.params,
                request_timeout,
            )
            .await
        }
        Some(AppServerRequestMethod::Initialize | AppServerRequestMethod::EventsSubscribe)
        | None => error_response(
            request.id,
            ErrorObject::new(
                ERROR_METHOD_NOT_FOUND,
                format!("未知方法: {}", request.method),
            ),
        ),
    }
}

async fn list_browser_tools(
    state: &ApiState,
    request_id: RequestId,
    params: Value,
) -> magi_app_server_protocol::ServerResponse {
    let params = match serde_json::from_value::<BrowserToolsListParams>(params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(
                    ERROR_INVALID_PARAMS,
                    format!("browser/tools/list 参数无效: {error}"),
                ),
            );
        }
    };
    let session_id = params
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(SessionId::new);
    let capability = state.browser_capability_snapshot(session_id.as_ref());
    let tools = capability
        .visible_tools()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name(),
                "access": tool.catalog_access(),
            })
        })
        .collect::<Vec<_>>();
    typed_success_from_json::<BrowserToolsListResult>(
        request_id,
        json!({
            "tools": tools,
            "capabilities": capability,
            "runtimeEpoch": state.runtime_epoch(),
        }),
        "browser/tools/list",
    )
}

struct BrowserToolTurnContext {
    session_id: SessionId,
    turn_id: String,
    source_thread_id: ThreadId,
    item_id: String,
}

fn browser_tool_turn_context(
    state: &ApiState,
    session_id: &SessionId,
    call_id: &str,
    tool: &str,
    arguments: &Value,
) -> Result<BrowserToolTurnContext, ErrorObject> {
    let sidecar = state
        .session_store
        .runtime_sidecar(session_id)
        .ok_or_else(|| ErrorObject::new(ERROR_INVALID_PARAMS, "会话尚未建立当前 Turn"))?;
    let turn = sidecar
        .current_turn
        .ok_or_else(|| ErrorObject::new(ERROR_INVALID_PARAMS, "会话尚未建立当前 Turn"))?;
    if !matches!(
        turn.status.trim().to_ascii_lowercase().as_str(),
        "pending" | "queued" | "accepted" | "running" | "started" | "streaming"
    ) {
        return Err(ErrorObject::new(
            ERROR_REQUEST_CONFLICT,
            format!("当前 Turn 已结束，不能执行浏览器工具: {}", turn.status),
        )
        .retryable(true));
    }
    let source_thread_id = state
        .session_store
        .orchestrator_thread_for_session(session_id)
        .map(|thread| thread.thread_id)
        .or_else(|| turn.items.first().map(|item| item.source_thread_id.clone()))
        .ok_or_else(|| ErrorObject::new(ERROR_INVALID_PARAMS, "会话尚未建立主线 thread"))?;
    let item_id = format!("browser-tool-{call_id}");
    let item = ActiveExecutionTurnItem {
        item_id: item_id.clone(),
        item_seq: 0,
        kind: "tool_call_started".to_string(),
        status: "running".to_string(),
        source: "browser".to_string(),
        title: Some(tool.to_string()),
        content: None,
        task_id: None,
        worker_id: None,
        role_id: None,
        tool_call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        tool_status: Some("running".to_string()),
        tool_arguments: Some(arguments.to_string()),
        tool_result: None,
        tool_error: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        metadata: std::collections::HashMap::from([
            ("source".to_string(), json!("browser")),
            ("browserTool".to_string(), json!(tool)),
        ]),
        timeline_entry_id: None,
        source_thread_id: source_thread_id.clone(),
    };
    let updated = state
        .session_store
        .upsert_current_turn_item_for_turn(session_id, Some(&turn.turn_id), item)
        .map_err(|error| {
            ErrorObject::new(ERROR_INTERNAL, format!("浏览器工具 Item 写入失败: {error}"))
        })?;
    if updated.is_none() {
        return Err(ErrorObject::new(
            ERROR_INVALID_PARAMS,
            "当前会话没有可写入的 Turn",
        ));
    }
    publish_browser_tool_item(state, session_id, &turn.turn_id, &item_id);
    Ok(BrowserToolTurnContext {
        session_id: session_id.clone(),
        turn_id: turn.turn_id,
        source_thread_id,
        item_id,
    })
}

fn browser_access_profile(profile: Option<BrowserAccessProfile>) -> AccessProfile {
    match profile {
        Some(BrowserAccessProfile::ReadOnly) => AccessProfile::ReadOnly,
        Some(BrowserAccessProfile::Restricted) => AccessProfile::Restricted,
        Some(BrowserAccessProfile::FullAccess) => AccessProfile::FullAccess,
        None => AccessProfile::FullAccess,
    }
}

fn publish_browser_tool_item(
    state: &ApiState,
    session_id: &SessionId,
    turn_id: &str,
    item_id: &str,
) {
    let canonical = state
        .session_store
        .canonical_turns_for_session(session_id)
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)
        .and_then(|turn| turn.items.into_iter().find(|item| item.item_id == item_id));
    let Some(canonical) = canonical else {
        return;
    };
    let now = UtcMillis::now();
    state.event_bus.publish(
        EventEnvelope::domain(
            EventId::new(format!("event-browser-item-{item_id}-{}", now.0)),
            "session.turn.item.upserted",
            json!({"source": "browser", "item": canonical}),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        }),
    );
}

async fn execute_browser_tool(
    state: &ApiState,
    request_id: RequestId,
    params: Value,
    control: RequestControl,
) -> magi_app_server_protocol::ServerResponse {
    let params = match serde_json::from_value::<BrowserToolParams>(params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(
                    ERROR_INVALID_PARAMS,
                    format!("browser/tool 参数无效: {error}"),
                ),
            );
        }
    };
    if params.arguments.is_empty() || params.tool.trim().is_empty() {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_INVALID_PARAMS, "tool 必须非空且 arguments 必须是对象"),
        );
    }
    let session_id = SessionId::new(params.session_id.trim());
    if state.session_store.session(&session_id).is_none() {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_SESSION_NOT_FOUND, "会话不存在"),
        );
    }
    let call_id = params
        .call_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("rpc-{}", request_id.as_str()));
    let turn_context = match browser_tool_turn_context(
        state,
        &session_id,
        &call_id,
        params.tool.trim(),
        &serde_json::to_value(&params.arguments).expect("浏览器工具 arguments 必须可序列化"),
    ) {
        Ok(context) => context,
        Err(error) => return error_response(request_id, error),
    };
    let workspace_id = params
        .workspace_id
        .map(|value| WorkspaceId::new(value.trim()));
    let capability_revision = params.browser_capability_revision.unwrap_or_else(|| {
        state
            .browser_capability_snapshot(Some(&session_id))
            .revision
    });
    let context = magi_tool_runtime::ToolExecutionContext {
        session_id: Some(session_id.clone()),
        workspace_id,
        task_id: params.task_id.map(magi_core::TaskId::new),
        worker_id: params.worker_id.map(magi_core::WorkerId::new),
        access_profile: browser_access_profile(params.access_profile),
        browser_capability_revision: Some(capability_revision),
        browser_execution_id: params.browser_execution_id,
        ..magi_tool_runtime::ToolExecutionContext::default()
    };
    let tool = params.tool.trim().to_string();
    let tool_for_item = tool.clone();
    let tool_for_runtime = tool.clone();
    let arguments =
        serde_json::to_string(&params.arguments).expect("浏览器工具 arguments 必须可序列化");
    let arguments_for_runtime = arguments.clone();
    let cancelled_before_execution = control.is_cancelled();
    let (payload, status) = if cancelled_before_execution {
        (
            json!({"status": "cancelled", "code": "request_cancelled", "message": "请求已取消"})
                .to_string(),
            ExecutionResultStatus::Cancelled,
        )
    } else {
        let runtime = state.browser_tool_runtime_dependencies();
        let call_id_for_runtime = ToolCallId::new(call_id.clone());
        let context_for_runtime = context.clone();
        match tokio::task::spawn_blocking(move || {
            runtime.execute(
                &call_id_for_runtime,
                &tool_for_runtime,
                &arguments_for_runtime,
                &context_for_runtime,
            )
        })
        .await
        {
            Ok(result) => result,
            Err(error) => (
                json!({"status": "failed", "code": "browser_runtime_join_failed", "message": error.to_string()}).to_string(),
                ExecutionResultStatus::Failed,
            ),
        }
    };
    let status_value = if cancelled_before_execution {
        "cancelled"
    } else {
        match status {
            ExecutionResultStatus::Succeeded => "completed",
            ExecutionResultStatus::NeedsApproval => "blocked",
            ExecutionResultStatus::Cancelled => "cancelled",
            ExecutionResultStatus::Rejected | ExecutionResultStatus::Failed => "failed",
        }
    };
    let status_value = if status_value == "failed"
        && serde_json::from_str::<Value>(&payload)
            .ok()
            .is_some_and(|value| {
                value.get("status").and_then(Value::as_str) == Some("indeterminate")
            }) {
        "indeterminate"
    } else {
        status_value
    };
    let item = ActiveExecutionTurnItem {
        item_id: turn_context.item_id.clone(),
        item_seq: 0,
        kind: "tool_call_result".to_string(),
        status: status_value.to_string(),
        source: "browser".to_string(),
        title: Some(tool_for_item.clone()),
        content: None,
        task_id: context.task_id.clone(),
        worker_id: context.worker_id.clone(),
        role_id: None,
        tool_call_id: Some(call_id),
        tool_name: Some(tool_for_item),
        tool_status: Some(status_value.to_string()),
        tool_arguments: Some(arguments.clone()),
        tool_result: (status_value == "completed").then_some(payload.clone()),
        tool_error: (status_value != "completed").then_some(payload.clone()),
        request_id: Some(request_id.as_str().to_string()),
        user_message_id: None,
        placeholder_message_id: None,
        metadata: std::collections::HashMap::from([
            ("source".to_string(), json!("browser")),
            ("browserTool".to_string(), json!(tool)),
        ]),
        timeline_entry_id: None,
        source_thread_id: turn_context.source_thread_id,
    };
    let updated = match state.session_store.upsert_current_turn_item_for_turn(
        &turn_context.session_id,
        Some(&turn_context.turn_id),
        item,
    ) {
        Ok(updated) => updated,
        Err(error) => {
            let protocol_error = match error {
                magi_core::DomainError::CurrentTurnConflict { .. } => ErrorObject::new(
                    ERROR_REQUEST_CONFLICT,
                    format!("浏览器工具结果归属的 Turn 已变化: {error}"),
                )
                .retryable(true),
                error => ErrorObject::new(
                    ERROR_INTERNAL,
                    format!("浏览器工具结果 Item 写入失败: {error}"),
                ),
            };
            return error_response(request_id, protocol_error);
        }
    };
    if updated.is_none() {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_REQUEST_CONFLICT, "浏览器工具结果写入时 Turn 已结束")
                .retryable(true),
        );
    }
    publish_browser_tool_item(
        state,
        &turn_context.session_id,
        &turn_context.turn_id,
        &turn_context.item_id,
    );
    if status_value == "cancelled" {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_REQUEST_CANCELLED, "请求已取消").retryable(false),
        );
    }
    typed_success_from_json::<magi_app_server_protocol::BrowserToolResult>(
        request_id,
        json!({
            "tool": tool,
            "status": status_value,
            "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::String(payload)),
            "itemId": turn_context.item_id,
            "turnId": turn_context.turn_id,
            "runtimeEpoch": state.runtime_epoch(),
        }),
        "browser/tool",
    )
}

struct ApprovalRequestContext<'a> {
    state: &'a ApiState,
    connection_state: &'a Arc<Mutex<ConnectionState>>,
    outgoing: &'a OutgoingChannels,
    pending_server_requests: &'a Arc<Mutex<HashMap<RequestId, oneshot::Sender<ClientResponse>>>>,
    next_server_request_id: &'a Arc<std::sync::atomic::AtomicU64>,
    control: RequestControl,
}

async fn request_approval(
    context: ApprovalRequestContext<'_>,
    request_id: RequestId,
    params: Value,
    request_timeout: std::time::Duration,
) -> magi_app_server_protocol::ServerResponse {
    let ApprovalRequestContext {
        state,
        connection_state,
        outgoing,
        pending_server_requests,
        next_server_request_id,
        control,
    } = context;
    let params = match serde_json::from_value::<ApprovalRequestParams>(params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(
                    ERROR_INVALID_PARAMS,
                    format!("approval/request 参数无效: {error}"),
                ),
            );
        }
    };
    if params.session_id.trim().is_empty() || params.reason.trim().is_empty() {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_INVALID_PARAMS, "sessionId 和 reason 不能为空"),
        );
    }
    if !connection_state.lock().await.capabilities.approvals {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_INVALID_PARAMS, "客户端未声明 approvals 能力"),
        );
    }
    let session_id = SessionId::new(params.session_id.trim());
    if state.session_store.session(&session_id).is_none() {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_SESSION_NOT_FOUND, "会话不存在"),
        );
    }
    let result = match request_client_approval(
        outgoing,
        pending_server_requests,
        next_server_request_id,
        ApprovalRequestParams {
            session_id: session_id.to_string(),
            reason: params.reason,
            risk: params.risk,
        },
        control,
        request_timeout,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return error_response(request_id, error),
    };
    typed_success(request_id, result)
}

async fn request_client_approval(
    outgoing: &OutgoingChannels,
    pending_server_requests: &Arc<Mutex<HashMap<RequestId, oneshot::Sender<ClientResponse>>>>,
    next_server_request_id: &Arc<std::sync::atomic::AtomicU64>,
    params: ApprovalRequestParams,
    control: RequestControl,
    request_timeout: std::time::Duration,
) -> Result<ApprovalDecision, ErrorObject> {
    if control.is_cancelled() {
        return Err(match control.termination() {
            RequestTermination::TimedOut => {
                ErrorObject::new(ERROR_REQUEST_TIMEOUT, "请求执行超时").retryable(true)
            }
            RequestTermination::Cancelled | RequestTermination::None => {
                ErrorObject::new(ERROR_REQUEST_CANCELLED, "请求已取消").retryable(false)
            }
        });
    }
    let server_request_id = RequestId::new(format!(
        "server-request-{}",
        next_server_request_id.fetch_add(1, Ordering::Relaxed)
    ))
    .expect("服务端请求 ID 必须非空");
    let (sender, receiver) = oneshot::channel();
    pending_server_requests
        .lock()
        .await
        .insert(server_request_id.clone(), sender);
    let request = ServerRequest {
        jsonrpc: Some(magi_app_server_protocol::JSONRPC_VERSION.to_string()),
        id: server_request_id.clone(),
        method: AppServerRequestMethod::ApprovalRequest.as_str().to_string(),
        request_timeout_ms: Some(request_timeout.as_millis().min(u64::MAX as u128) as u64),
        params: serde_json::to_value(params).map_err(|error| {
            ErrorObject::new(ERROR_INTERNAL, format!("审批请求序列化失败: {error}"))
        })?,
    };
    if try_send_json(outgoing, request).is_err() {
        pending_server_requests
            .lock()
            .await
            .remove(&server_request_id);
        return Err(ErrorObject::new(ERROR_INTERNAL, "无法发送服务端请求"));
    }
    let response = tokio::select! {
        _ = control.notify.notified() => {
            pending_server_requests.lock().await.remove(&server_request_id);
            return Err(ErrorObject::new(ERROR_REQUEST_CANCELLED, "请求已取消").retryable(false));
        }
        result = tokio::time::timeout(request_timeout, receiver) => {
            match result {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => {
                    pending_server_requests.lock().await.remove(&server_request_id);
                    return Err(ErrorObject::new(ERROR_INTERNAL, "服务端请求响应通道已关闭"));
                }
                Err(_) => {
                    pending_server_requests.lock().await.remove(&server_request_id);
                    return Err(ErrorObject::new(ERROR_REQUEST_TIMEOUT, "服务端请求响应超时").retryable(true));
                }
            }
        }
    };
    if let Some(error) = response.error {
        return Err(error);
    }
    let result = response.result.unwrap_or(Value::Null);
    serde_json::from_value(result).map_err(|error| {
        ErrorObject::new(
            ERROR_INTERNAL,
            format!("审批响应不符合协议 Schema: {error}"),
        )
    })
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
    let sessions = sessions
        .into_iter()
        .map(|session| session_summary(state, session))
        .collect::<Vec<_>>();
    typed_success(
        request_id,
        SessionListResult {
            sessions,
            runtime_epoch: state.runtime_epoch().to_string(),
        },
    )
}

fn session_summary(state: &ApiState, session: magi_session_store::SessionRecord) -> SessionSummary {
    let session_id = session.session_id.clone();
    let is_running = state
        .session_store
        .runtime_sidecar(&session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| {
            !matches!(
                turn.status.trim().to_ascii_lowercase().as_str(),
                "completed" | "blocked" | "failed" | "interrupted" | "cancelled" | "superseded"
            )
        });
    let summary =
        SessionDirectoryEntryDto::from_record(session, is_running, usize::from(is_running));
    SessionSummary {
        session_id: summary.session_id,
        workspace_id: summary.workspace_id,
        title: summary.title,
        status: summary.status,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        message_count: summary.message_count,
        is_running: summary.is_running,
        running_task_count: summary.running_task_count,
        has_unread_completion: summary.has_unread_completion,
    }
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
        match serde_json::to_value(state.session_store.canonical_turns_for_session(&session_id)) {
            Ok(turns) => turns,
            Err(error) => {
                return error_response(
                    request_id,
                    ErrorObject::new(
                        ERROR_INTERNAL,
                        format!("session/read Turn 序列化失败: {error}"),
                    ),
                );
            }
        }
    } else {
        json!([])
    };
    typed_success_from_json::<SessionReadResult>(
        request_id,
        json!({
            "session": session_summary(state, session),
            "turns": turns,
            "runtimeEpoch": state.runtime_epoch(),
        }),
        "session/read",
    )
}

async fn start_turn(
    state: &ApiState,
    request_id: RequestId,
    params: Value,
) -> magi_app_server_protocol::ServerResponse {
    let protocol_params = match serde_json::from_value::<TurnStartParams>(params) {
        Ok(params) => params,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(ERROR_INVALID_PARAMS, error.to_string()),
            );
        }
    };
    let request = match serde_json::to_value(protocol_params)
        .map_err(|error| error.to_string())
        .and_then(|value| {
            serde_json::from_value::<crate::dto::SessionTurnRequestDto>(value)
                .map_err(|error| error.to_string())
        }) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(ERROR_INTERNAL, format!("turn/start 参数转换失败: {error}")),
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
        typed_success_from_json::<TurnStartResult>(
            request_id,
            json!({
                    "kind": "accepted",
                    "replayed": true,
                    "requestId": request.request_id(),
                    "entryId": format!("timeline-{}-{}", existing_turn.session_id, existing_turn.accepted_at.0),
                    "eventId": format!("event-session-turn-task-{}", existing_turn.accepted_at.0),
                    "sessionId": existing_turn.session_id,
                    "turnId": existing_turn.turn_id,
                    "acceptedAt": existing_turn.accepted_at,
                    "createdSession": false,
                    "route": user_message_item.and_then(|item| {
                        item.metadata.get("route").and_then(Value::as_str)
                    }),
                    "userMessageItemId": user_message_item.map(|item| item.item_id.clone()),
                    "runtimeEpoch": state.runtime_epoch(),
                    "eventStreamNextSequence": state.event_bus.snapshot().next_sequence,
                    "canonicalTurn": existing_turn,
                    "canonicalItem": user_message_item,
                    "sessionSummary": null,
                    "queue": null,
            }),
            "turn/start",
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
        typed_success_from_json::<TurnStartResult>(
            request_id,
            json!({
                    "kind": "queued",
                    "replayed": true,
                    "requestId": queued_turn.request.request_id(),
                    "entryId": queue_id,
                    "eventId": format!("event-session-turn-queued-{}", queued_turn.accepted_at.0),
                    "sessionId": queued_turn.session_id,
                    "turnId": null,
                    "acceptedAt": queued_turn.accepted_at,
                    "route": queued_turn.route,
                    "createdSession": false,
                    "userMessageItemId": user_message_item_id,
                    "runtimeEpoch": state.runtime_epoch(),
                    "eventStreamNextSequence": state.event_bus.snapshot().next_sequence,
                    "sessionSummary": null,
                    "canonicalTurn": null,
                    "canonicalItem": null,
                    "queue": {
                        "queueId": queued_turn.queue_id,
                        "queuePosition": queue_position,
                    },
            }),
            "turn/start",
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
                let mut result = match serde_json::to_value(result) {
                    Ok(result) => result,
                    Err(error) => {
                        return error_response(
                            request_id,
                            ErrorObject::new(
                                ERROR_INTERNAL,
                                format!("turn/start 结果序列化失败: {error}"),
                            ),
                        );
                    }
                };
                let queued = result
                    .get("queued")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let queue = if queued {
                    json!({
                        "queueId": result.get("queueId").cloned().unwrap_or(Value::Null),
                        "queuePosition": result.get("queuePosition").cloned().unwrap_or(Value::Null),
                    })
                } else {
                    Value::Null
                };
                let Some(object) = result.as_object_mut() else {
                    return error_response(
                        request_id,
                        ErrorObject::new(ERROR_INTERNAL, "turn/start 结果必须是对象"),
                    );
                };
                object.remove("queued");
                object.remove("queueId");
                object.remove("queuePosition");
                object.insert(
                    "kind".to_string(),
                    json!(if queued { "queued" } else { "accepted" }),
                );
                object.insert("replayed".to_string(), json!(false));
                object.insert("requestId".to_string(), json!(business_request_id));
                let turn_id = object
                    .get("canonicalTurn")
                    .and_then(|turn| turn.get("turnId"))
                    .cloned()
                    .unwrap_or(Value::Null);
                object.insert("turnId".to_string(), turn_id);
                for field in [
                    "sessionSummary",
                    "userMessageItemId",
                    "canonicalTurn",
                    "canonicalItem",
                ] {
                    object.entry(field.to_string()).or_insert(Value::Null);
                }
                object.insert("queue".to_string(), queue);
                typed_success_from_json::<TurnStartResult>(request_id, result, "turn/start")
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

async fn handle_cancel_notification(
    pending_requests: &Arc<Mutex<HashMap<RequestId, RequestControl>>>,
    message: ClientNotification,
) {
    let Ok(params) = serde_json::from_value::<CancelRequestParams>(message.params) else {
        return;
    };
    if let Some(control) = pending_requests.lock().await.get(&params.id).cloned() {
        control.cancel();
    }
}

async fn resolve_server_response(
    pending_server_requests: &Arc<Mutex<HashMap<RequestId, oneshot::Sender<ClientResponse>>>>,
    response: ClientResponse,
) -> bool {
    let sender = pending_server_requests.lock().await.remove(&response.id);
    let Some(sender) = sender else {
        return false;
    };
    sender.send(response).is_ok()
}

fn request_timeout(request: &ClientRequest) -> Result<std::time::Duration, ErrorObject> {
    let requested = request
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    if !(MIN_REQUEST_TIMEOUT_MS..=MAX_REQUEST_TIMEOUT_MS).contains(&requested) {
        return Err(ErrorObject::new(
            ERROR_INVALID_PARAMS,
            format!(
                "requestTimeoutMs 必须在 {MIN_REQUEST_TIMEOUT_MS} 到 {MAX_REQUEST_TIMEOUT_MS} 毫秒之间"
            ),
        ));
    }
    Ok(std::time::Duration::from_millis(requested))
}

fn browser_tool_retryable(request: &ClientRequest) -> bool {
    let Ok(params) = serde_json::from_value::<BrowserToolParams>(request.params.clone()) else {
        return false;
    };
    let Some(tool) = BrowserToolKind::from_name(params.tool.trim()) else {
        return false;
    };
    matches!(tool.catalog_access(), BrowserToolAccess::Read)
}

fn browser_timeout_error(retryable: bool) -> ErrorObject {
    let mut error = ErrorObject::new(
        ERROR_REQUEST_TIMEOUT,
        if retryable {
            "浏览器只读操作执行超时，可以安全重试"
        } else {
            "浏览器操作执行超时，副作用状态不确定，禁止自动重放"
        },
    )
    .retryable(retryable);
    error.data = Some(json!({
        "status": "indeterminate",
        "operation": "browser/tool",
        "retryable": retryable,
    }));
    error
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
        ActiveExecutionTurn, ActiveExecutionTurnItem, CanonicalTurn, CanonicalTurnItem,
        CanonicalTurnItemKind, CanonicalTurnItemStatus, CanonicalTurnStatus,
        CanonicalTurnVisibility, SessionStore, TimelineEntryInput, TimelineEntryKind,
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

    #[tokio::test]
    async fn start_turn_replays_persisted_canonical_turn_without_submitting_again() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(8));
        let session_store = Arc::new(SessionStore::default());
        let session_id = SessionId::new("session-app-server-canonical-replay");
        session_store
            .create_session_for_workspace_at(
                session_id.clone(),
                "Canonical replay",
                None,
                UtcMillis(1),
            )
            .expect("重放测试会话应创建");
        let request: crate::dto::SessionTurnRequestDto = serde_json::from_value(json!({
            "sessionId": session_id,
            "scope": "personal",
            "text": "inspect browser",
            "requestId": "request-canonical-replay"
        }))
        .expect("重放请求应反序列化");
        let request_fingerprint = request.request_fingerprint().expect("重放请求指纹应生成");
        let item = ActiveExecutionTurnItem {
            item_id: "user-canonical-replay".to_string(),
            item_seq: 0,
            kind: "user_message".to_string(),
            status: "completed".to_string(),
            source: "user".to_string(),
            title: None,
            content: request.trimmed_text(),
            task_id: None,
            worker_id: None,
            role_id: None,
            tool_call_id: None,
            tool_name: None,
            tool_status: None,
            tool_arguments: None,
            tool_result: None,
            tool_error: None,
            request_id: request.request_id(),
            user_message_id: None,
            placeholder_message_id: None,
            metadata: HashMap::from([
                ("route".to_string(), json!("chat")),
                ("requestFingerprint".to_string(), json!(request_fingerprint)),
            ]),
            timeline_entry_id: Some("timeline-canonical-replay".to_string()),
            source_thread_id: ThreadId::new("thread-canonical-replay"),
        };
        session_store
            .accept_current_turn_with_timeline_entry(
                session_id.clone(),
                TimelineEntryInput::new(
                    "timeline-canonical-replay",
                    TimelineEntryKind::UserMessage,
                    "inspect browser",
                    UtcMillis(2),
                ),
                ActiveExecutionTurn {
                    turn_id: "turn-canonical-replay".to_string(),
                    turn_seq: 1,
                    accepted_at: UtcMillis(2),
                    completed_at: None,
                    status: "running".to_string(),
                    user_message: Some("inspect browser".to_string()),
                    items: vec![item],
                },
            )
            .expect("canonical Turn 应持久化");
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );

        let response = start_turn_once(
            &state,
            RequestId::new("rpc-canonical-replay").expect("RPC request id 合法"),
            request,
        )
        .await;
        let response = serde_json::to_value(response).expect("重放响应应可序列化");
        assert_eq!(response["result"]["replayed"], true);
        assert_eq!(response["result"]["turnId"], "turn-canonical-replay");
        assert_eq!(
            response["result"]["userMessageItemId"],
            "user-canonical-replay"
        );
        assert_eq!(
            state
                .session_store
                .canonical_turns_for_session(&SessionId::new("session-app-server-canonical-replay"))
                .len(),
            1,
            "重试只能读取已持久化的 canonical Turn，不能再次提交"
        );
    }

    #[tokio::test]
    async fn start_turn_replays_persisted_queue_without_enqueuing_again() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(8));
        let session_store = Arc::new(SessionStore::default());
        let state = ApiState::new(
            "magi-test",
            event_bus,
            session_store,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
        let session_id = SessionId::new("session-app-server-queue-replay");
        let request: crate::dto::SessionTurnRequestDto = serde_json::from_value(json!({
            "sessionId": session_id,
            "scope": "personal",
            "text": "queued browser check",
            "requestId": "request-queue-replay"
        }))
        .expect("排队请求应反序列化");
        let request_fingerprint = request.request_fingerprint().expect("排队请求指纹应生成");
        let queue_id = "queue-app-server-replay".to_string();
        state
            .enqueue_regular_session_turn(crate::state::QueuedRegularSessionTurn {
                request,
                request_fingerprint: Some(request_fingerprint),
                requested_workspace_id: None,
                accepted_at: UtcMillis(3),
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
                session_id: session_id.clone(),
                workspace_id: None,
                queue_id: queue_id.clone(),
                retry_count: 0,
            })
            .expect("排队 Turn 应持久化");

        let replay_request: crate::dto::SessionTurnRequestDto = serde_json::from_value(json!({
            "sessionId": session_id,
            "scope": "personal",
            "text": "queued browser check",
            "requestId": "request-queue-replay"
        }))
        .expect("重放排队请求应反序列化");
        let response = start_turn_once(
            &state,
            RequestId::new("rpc-queue-replay").expect("RPC request id 合法"),
            replay_request,
        )
        .await;
        let response = serde_json::to_value(response).expect("排队重放响应应可序列化");
        assert_eq!(response["result"]["replayed"], true);
        assert_eq!(response["result"]["kind"], "queued");
        assert_eq!(response["result"]["queue"]["queueId"], queue_id);
        assert_eq!(
            state.queued_regular_session_turn_count(&SessionId::new(
                "session-app-server-queue-replay"
            )),
            1,
            "排队请求重试不能再次入队"
        );
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
                    "capabilities": {"streaming": true, "approvals": true}
                }
            }),
            "1",
        )
        .await;
        assert_eq!(initialize["jsonrpc"], "2.0");
        assert_eq!(initialize["id"], 1);
        assert_eq!(initialize["result"]["capabilities"]["events"], true);
        assert_eq!(initialize["result"]["capabilities"]["browserTools"], true);

        socket
            .send(ClientMessage::Text(
                serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("initialized 通知应发送");

        socket
            .send(ClientMessage::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "approval-rpc-1",
                    "method": "approval/request",
                    "params": {
                        "sessionId": session_id,
                        "reason": "验证双向服务端请求",
                        "risk": "high"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("approval 请求应发送");
        let server_request = next_text_message(&mut socket).await;
        assert_eq!(server_request["method"], "approval/request");
        let server_request_id = server_request["id"].clone();
        socket
            .send(ClientMessage::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": server_request_id,
                    "result": {"approved": true}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("approval 响应应发送");
        let approval_response = loop {
            let response = next_text_message(&mut socket).await;
            if response["id"] == "approval-rpc-1" {
                break response;
            }
        };
        assert_eq!(approval_response["result"]["approved"], true);

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

    #[tokio::test]
    async fn websocket_subscription_reports_resync_for_an_expired_cursor() {
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(2));
        for sequence in 1..=4 {
            event_bus.publish(event(
                sequence,
                Some("session-resync"),
                Some("workspace-resync"),
            ));
        }
        let state = ApiState::new(
            "magi-test",
            event_bus.clone(),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        );
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
        let initialize = send_request(
            &mut socket,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": "resync-initialize",
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "resync-integration-test"},
                    "protocol": {"major": 1, "minor": 0},
                    "capabilities": {"streaming": true}
                }
            }),
            "resync-initialize",
        )
        .await;
        assert_eq!(initialize["result"]["capabilities"]["events"], true);
        socket
            .send(ClientMessage::Text(
                serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("initialized 通知应发送");

        socket
            .send(ClientMessage::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "resync-subscribe",
                    "method": "events/subscribe",
                    "params": {
                        "workspaceId": "workspace-resync",
                        "afterSequence": 1
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("events/subscribe 请求应发送");

        let mut response = None;
        let mut resync_notification = None;
        for _ in 0..2 {
            let message = next_text_message(&mut socket).await;
            if message.get("id").and_then(Value::as_str) == Some("resync-subscribe") {
                response = Some(message);
            } else if message.get("method").and_then(Value::as_str) == Some("events/resyncRequired")
                && message["params"]["reason"] == "afterSequenceExpired"
            {
                resync_notification = Some(message);
            }
            if response.is_some() && resync_notification.is_some() {
                break;
            }
        }
        let response = response.expect("events/subscribe 应返回响应");
        assert_eq!(response["result"]["resyncRequired"], true);
        let resync_notification = resync_notification.expect("过期游标应发送 resync 通知");
        assert_eq!(resync_notification["params"]["requestedAfterSequence"], 1);
        assert_eq!(
            resync_notification["params"]["snapshot"]["recent_events"]
                .as_array()
                .map(Vec::len),
            Some(2),
            "resync 快照应包含保留窗口内的事件"
        );

        drop(socket);
        shutdown_tx.send(()).expect("测试服务应收到停止信号");
        server.await.expect("测试服务任务应正常结束");
    }
}
