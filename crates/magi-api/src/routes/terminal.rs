use axum::{
    Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{delete, get},
};
use futures_util::{SinkExt, StreamExt};
use portable_pty::PtySize;
use serde::Deserialize;

use super::session_scope;
use crate::{
    errors::ApiError,
    state::ApiState,
    terminal_runtime::{
        TerminalBinding, TerminalEvent, TerminalLifecycle, TerminalRuntimeError, TerminalSession,
    },
};

const TERMINAL_INPUT_MAX_BYTES: usize = 64 * 1024;
const TERMINAL_ID_MAX_BYTES: usize = 128;
const TERMINAL_MIN_COLS: u16 = 2;
const TERMINAL_MAX_COLS: u16 = 512;
const TERMINAL_MIN_ROWS: u16 = 2;
const TERMINAL_MAX_ROWS: u16 = 256;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/terminal/sessions/{terminal_tab_id}/channel",
            get(terminal_channel),
        )
        .route(
            "/terminal/sessions/{terminal_tab_id}",
            delete(close_terminal),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalScopeQuery {
    session_id: String,
    workspace_id: String,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default = "default_terminal_cols")]
    cols: u16,
    #[serde(default = "default_terminal_rows")]
    rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TerminalClientMessage {
    Resize { cols: u16, rows: u16 },
}

fn default_terminal_cols() -> u16 {
    80
}

fn default_terminal_rows() -> u16 {
    24
}

async fn terminal_channel(
    headers: HeaderMap,
    State(state): State<ApiState>,
    Path(terminal_tab_id): Path<String>,
    Query(query): Query<TerminalScopeQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    ensure_local_terminal_access(&headers)?;
    let binding = resolve_terminal_binding(&state, &terminal_tab_id, &query)?;
    let size = terminal_size(query.cols, query.rows);
    let terminal = state
        .terminal_sessions
        .open_or_create(binding, size)
        .map_err(terminal_runtime_error)?;
    Ok(ws.on_upgrade(move |socket| run_terminal_channel(socket, terminal)))
}

async fn close_terminal(
    headers: HeaderMap,
    State(state): State<ApiState>,
    Path(terminal_tab_id): Path<String>,
    Query(query): Query<TerminalScopeQuery>,
) -> Result<StatusCode, ApiError> {
    ensure_local_terminal_access(&headers)?;
    let binding = resolve_terminal_binding(&state, &terminal_tab_id, &query)?;
    state
        .terminal_sessions
        .close(&binding)
        .map_err(terminal_runtime_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn resolve_terminal_binding(
    state: &ApiState,
    terminal_tab_id: &str,
    query: &TerminalScopeQuery,
) -> Result<TerminalBinding, ApiError> {
    validate_terminal_tab_id(terminal_tab_id)?;
    let scope = session_scope::require_session_workspace_scope(
        state,
        Some(&query.session_id),
        Some(&query.workspace_id),
        query.workspace_path.as_deref(),
        "打开本地终端",
    )?;
    Ok(TerminalBinding {
        terminal_tab_id: terminal_tab_id.to_string(),
        workspace_id: scope.workspace_id.to_string(),
        workspace_path: scope.workspace_path.into(),
        session_id: scope.session_id.to_string(),
    })
}

fn validate_terminal_tab_id(value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > TERMINAL_ID_MAX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::InvalidInput("terminalTabId 格式无效".to_string()));
    }
    Ok(())
}

fn terminal_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols: cols.clamp(TERMINAL_MIN_COLS, TERMINAL_MAX_COLS),
        rows: rows.clamp(TERMINAL_MIN_ROWS, TERMINAL_MAX_ROWS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn terminal_runtime_error(error: TerminalRuntimeError) -> ApiError {
    match error {
        TerminalRuntimeError::BindingMismatch => ApiError::Conflict(error.to_string()),
        TerminalRuntimeError::Start(_)
        | TerminalRuntimeError::Write(_)
        | TerminalRuntimeError::Resize(_) => ApiError::InternalAssemblyError(error.to_string()),
    }
}

fn ensure_local_terminal_access(headers: &HeaderMap) -> Result<(), ApiError> {
    if super::is_public_tunnel_request(headers) {
        return Err(ApiError::Conflict(
            "远程公网访问不能打开本机终端".to_string(),
        ));
    }
    Ok(())
}

async fn run_terminal_channel(socket: WebSocket, terminal: std::sync::Arc<TerminalSession>) {
    let mut events = terminal.subscribe();
    let snapshot = terminal.snapshot();
    let snapshot_sequence = snapshot.sequence;
    let (mut sink, mut source) = socket.split();

    if send_lifecycle(&mut sink, &snapshot.lifecycle)
        .await
        .is_err()
    {
        return;
    }
    if !snapshot.output.is_empty()
        && sink
            .send(Message::Binary(snapshot.output.into()))
            .await
            .is_err()
    {
        return;
    }
    if snapshot.lifecycle.is_terminal() {
        return;
    }

    loop {
        tokio::select! {
            incoming = source.next() => {
                let Some(Ok(message)) = incoming else { break; };
                match message {
                    Message::Binary(bytes) => {
                        if bytes.len() > TERMINAL_INPUT_MAX_BYTES {
                            if send_error(&mut sink, "terminal_input_too_large", "单次终端输入不能超过 64 KiB").await.is_err() {
                                break;
                            }
                            continue;
                        }
                        if let Err(error) = terminal.write_input(&bytes)
                            && send_error(&mut sink, "terminal_input_failed", &error.to_string()).await.is_err()
                        {
                            break;
                        }
                    }
                    Message::Text(text) => {
                        let Ok(message) = serde_json::from_str::<TerminalClientMessage>(&text) else {
                            if send_error(&mut sink, "terminal_message_invalid", "终端控制消息格式无效").await.is_err() {
                                break;
                            }
                            continue;
                        };
                        let TerminalClientMessage::Resize { cols, rows } = message;
                        if let Err(error) = terminal.resize(terminal_size(cols, rows))
                            && send_error(&mut sink, "terminal_resize_failed", &error.to_string()).await.is_err()
                        {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(bytes) => {
                        if sink.send(Message::Pong(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) if event.sequence() <= snapshot_sequence => continue,
                    Ok(TerminalEvent::Output { bytes, .. }) => {
                        if sink.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(TerminalEvent::Lifecycle { lifecycle, .. }) => {
                        let terminal_state = lifecycle.is_terminal();
                        if send_lifecycle(&mut sink, &lifecycle).await.is_err() || terminal_state {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let _ = send_error(
                            &mut sink,
                            "terminal_resync_required",
                            "终端输出过快，正在重新同步",
                        ).await;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn send_lifecycle(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    lifecycle: &TerminalLifecycle,
) -> Result<(), axum::Error> {
    let payload = match lifecycle {
        TerminalLifecycle::Running => serde_json::json!({ "type": "ready" }),
        TerminalLifecycle::Exited { exit_code, signal } => serde_json::json!({
            "type": "exit",
            "exitCode": exit_code,
            "signal": signal,
        }),
        TerminalLifecycle::Failed { message } => serde_json::json!({
            "type": "error",
            "code": "terminal_runtime_failed",
            "message": message,
        }),
        TerminalLifecycle::Closed => serde_json::json!({ "type": "closed" }),
    };
    sink.send(Message::Text(payload.to_string().into())).await
}

async fn send_error(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: &str,
    message: &str,
) -> Result<(), axum::Error> {
    sink.send(Message::Text(
        serde_json::json!({
            "type": "error",
            "code": code,
            "message": message,
        })
        .to_string()
        .into(),
    ))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_tab_id_rejects_path_and_query_syntax() {
        assert!(validate_terminal_tab_id("terminal-1_2.3").is_ok());
        assert!(validate_terminal_tab_id("../terminal").is_err());
        assert!(validate_terminal_tab_id("terminal?token=1").is_err());
        assert!(validate_terminal_tab_id("").is_err());
    }

    #[test]
    fn terminal_size_is_bounded() {
        assert_eq!(terminal_size(0, 0).cols, TERMINAL_MIN_COLS);
        assert_eq!(terminal_size(u16::MAX, u16::MAX).rows, TERMINAL_MAX_ROWS);
    }
}
