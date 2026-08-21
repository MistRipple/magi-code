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
    ERROR_METHOD_NOT_FOUND, ERROR_NOT_INITIALIZED, ERROR_SERVER_OVERLOADED, ErrorObject,
    InitializeParams, InitializeResult, ProtocolVersion, RequestId, ServerCapabilities,
    classify_client_message, error_response, notification, response,
};
use magi_core::{SessionId, WorkspaceId};
use magi_event_bus::{EventEnvelope, EventStreamSnapshot};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore, broadcast, mpsc};

use crate::{errors::ApiError, routes::sessions, state::ApiState};

const MAX_IN_FLIGHT_REQUESTS: usize = 32;
const OUTGOING_QUEUE_CAPACITY: usize = 64;

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

type OutgoingSender = mpsc::Sender<String>;

async fn run_connection(socket: WebSocket, state: ApiState) {
    let (mut sink, mut stream) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<String>(OUTGOING_QUEUE_CAPACITY);
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            if sink.send(Message::Text(message.into())).await.is_err() {
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
                                    &outgoing_tx,
                                    None,
                                    ERROR_INVALID_REQUEST,
                                    format!("JSON 无效: {error}"),
                                    false,
                                ).await;
                                continue;
                            }
                        };
                        let message = match classify_client_message(&value) {
                            Ok(message) => message,
                            Err(error) => {
                                let _ = send_protocol_error(
                                    &outgoing_tx,
                                    None,
                                    ERROR_INVALID_REQUEST,
                                    error.to_string(),
                                    false,
                                ).await;
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
                                if send_json(&outgoing_tx, response).await.is_err() {
                                    break;
                                }
                            }
                            ClientMessage::Request(request) if request.method == "events/subscribe" => {
                                let response = subscribe_events(
                                    &state,
                                    &connection_state,
                                    &mut initial_snapshot,
                                    request,
                                    &outgoing_tx,
                                ).await;
                                if send_json(&outgoing_tx, response).await.is_err() {
                                    break;
                                }
                            }
                            ClientMessage::Request(request) => {
                                let slot = match request_slots.clone().try_acquire_owned() {
                                    Ok(slot) => slot,
                                    Err(_) => {
                                        let _ = send_json(
                                            &outgoing_tx,
                                            error_response(
                                                request.id,
                                                ErrorObject::new(
                                                    ERROR_SERVER_OVERLOADED,
                                                    "服务端请求过多，请稍后重试",
                                                ).retryable(true),
                                            ),
                                        ).await;
                                        continue;
                                    }
                                };
                                let state_for_request = state.clone();
                                let connection_for_request = connection_state.clone();
                                let outgoing_for_request = outgoing_tx.clone();
                                request_tasks.spawn(async move {
                                    let _slot = slot;
                                    let response = dispatch_request(
                                        &state_for_request,
                                        &connection_for_request,
                                        request,
                                    ).await;
                                    let _ = send_json(&outgoing_for_request, response).await;
                                });
                            }
                            ClientMessage::Notification(notification) => {
                                handle_notification(&connection_state, notification).await;
                            }
                            ClientMessage::Response(_) => {
                                let _ = send_protocol_error(
                                    &outgoing_tx,
                                    None,
                                    ERROR_INVALID_REQUEST,
                                    "当前连接没有待处理的服务端请求".to_string(),
                                    false,
                                ).await;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        let _ = payload;
                        let _ = outgoing_tx.send("{\"type\":\"pong\"}".to_string()).await;
                    }
                    Message::Close(_) => break,
                    Message::Binary(_) | Message::Pong(_) => {}
                }
            }
            event = event_rx.recv() => {
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
                            && send_json(
                                &outgoing_tx,
                                notification(
                                    format!("event/{}", event.event_type),
                                    json!({"sequence": event.sequence, "event": event}),
                                ),
                            )
                            .await
                            .is_err()
                        {
                            connection_state.lock().await.subscription.after_sequence = event.sequence;
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let (snapshot, replacement) = state.event_bus.snapshot_and_subscribe();
                        event_rx = replacement;
                        if is_subscribed(&connection_state).await {
                            connection_state.lock().await.subscription.after_sequence =
                                snapshot.next_sequence.saturating_sub(1);
                            let _ = send_json(&outgoing_tx, notification(
                                "events/resyncRequired",
                                json!({
                                    "skipped": skipped,
                                    "snapshot": filtered_snapshot(&snapshot, &connection_state).await,
                                }),
                            )).await;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    request_tasks.abort_all();
    while request_tasks.join_next().await.is_some() {}
    drop(outgoing_tx);
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
                browser_tools: true,
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
    outgoing: &OutgoingSender,
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
            after_sequence: after_sequence,
        };
    }
    let snapshot = initial_snapshot
        .take()
        .unwrap_or_else(|| state.event_bus.snapshot());
    let snapshot = filter_snapshot(snapshot, after_sequence, connection_state).await;
    let _ = send_json(
        outgoing,
        notification(
            "events/snapshot",
            serde_json::to_value(&snapshot).unwrap_or_else(|_| json!({})),
        ),
    )
    .await;
    response(
        request.id,
        json!({"subscribed": true, "nextSequence": snapshot.next_sequence}),
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
        return error_response(request_id, ErrorObject::new(-32004, "会话不存在"));
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
    mut params: Value,
) -> magi_app_server_protocol::ServerResponse {
    let Some(object) = params.as_object_mut() else {
        return error_response(
            request_id,
            ErrorObject::new(ERROR_INVALID_PARAMS, "turn/start 参数必须是对象"),
        );
    };
    // 协议 request id 是重试相关性的唯一最低保证。若调用方没有业务 requestId，
    // 使用同一个 id 重试时会命中现有 session turn 的幂等识别逻辑。
    object
        .entry("requestId")
        .or_insert_with(|| json!(request_id.as_str()));
    let request = match serde_json::from_value::<crate::dto::SessionTurnRequestDto>(params) {
        Ok(request) => request,
        Err(error) => {
            return error_response(
                request_id,
                ErrorObject::new(ERROR_INVALID_PARAMS, error.to_string()),
            );
        }
    };
    match sessions::submit_session_turn(axum::extract::State(state.clone()), axum::Json(request))
        .await
    {
        Ok(axum::Json(result)) => response(
            request_id,
            serde_json::to_value(result).unwrap_or_else(|_| json!({})),
        ),
        Err(error) => error_response(request_id, api_error_to_protocol(error)),
    }
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
        return;
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
            ErrorObject::new(-32004, message)
        }
        ApiError::RecoveryNotFound(message) => ErrorObject::new(-32004, message),
        ApiError::Conflict(message) | ApiError::TurnConflict { message, .. } => {
            ErrorObject::new(-32010, message).retryable(true)
        }
        ApiError::CapabilityUnavailable { message, .. } => {
            ErrorObject::new(-32011, message).retryable(false)
        }
        ApiError::ModelInvocationFailed(message) | ApiError::InternalAssemblyError(message) => {
            ErrorObject::new(ERROR_INTERNAL, message).retryable(true)
        }
    }
}

async fn send_json<T: serde::Serialize>(
    outgoing: &OutgoingSender,
    value: T,
) -> Result<(), mpsc::error::SendError<String>> {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| {
        serde_json::to_string(&error_response(
            RequestId::new("serialization-error").expect("固定 request id 合法"),
            ErrorObject::new(ERROR_INTERNAL, "响应序列化失败"),
        ))
        .expect("内部错误响应必须可序列化")
    });
    outgoing.send(text).await
}

async fn send_protocol_error(
    outgoing: &OutgoingSender,
    request_id: Option<RequestId>,
    code: i32,
    message: String,
    retryable: bool,
) -> Result<(), mpsc::error::SendError<String>> {
    let id = request_id
        .unwrap_or_else(|| RequestId::new("protocol-error").expect("固定 request id 合法"));
    send_json(
        outgoing,
        error_response(id, ErrorObject::new(code, message).retryable(retryable)),
    )
    .await
}
