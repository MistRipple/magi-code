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

use crate::scope_binding as session_scope;
use crate::scope_binding::resolve_project_source_directory;
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
    scope: crate::dto::SessionScopeKindDto,
    session_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    source_folder_id: Option<String>,
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
    let _project_admission = binding
        .workspace_id
        .as_ref()
        .map(|id| state.try_project_directory_admission(&magi_core::WorkspaceId::new(id)))
        .transpose()?;
    // 解析可能早于项目编辑提交；准入锁内重新核对原目录映射。
    validate_terminal_binding(&state, &binding)?;
    let size = terminal_size(query.cols, query.rows);
    let terminal = state
        .terminal_sessions
        .open_or_create(binding, size)
        .map_err(terminal_runtime_error)?;
    Ok(ws.on_upgrade(move |socket| run_terminal_channel(socket, terminal, state)))
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
    let session_id = session_scope::parse_session_id(Some(&query.session_id))?;
    let request_scope = session_scope::require_session_request_scope(
        state,
        Some(session_id.as_str()),
        query.scope,
        query.workspace_id.as_deref(),
        query.workspace_path.as_deref(),
    )?;
    let (execution_root, source_folder_id) = match &request_scope.scope {
        session_scope::SessionScope::Personal if query.source_folder_id.is_some() => {
            return Err(ApiError::InvalidInput(
                "个人会话不能选择项目源文件夹".into(),
            ));
        }
        session_scope::SessionScope::Personal => {
            (state.personal_session_execution_root(&session_id)?, None)
        }
        session_scope::SessionScope::Workspace(binding) => {
            let directory = resolve_project_source_directory(
                state,
                &binding.workspace_id,
                Some(&session_id),
                query.source_folder_id.as_deref(),
            )?;
            (directory.execution_root, Some(directory.source_folder_id))
        }
    };
    Ok(TerminalBinding {
        terminal_tab_id: terminal_tab_id.to_string(),
        workspace_id: request_scope
            .workspace_id()
            .map(|workspace_id| workspace_id.to_string()),
        execution_root,
        source_folder_id,
        session_id: session_id.to_string(),
    })
}

fn validate_terminal_binding(state: &ApiState, binding: &TerminalBinding) -> Result<(), ApiError> {
    let session_id = magi_core::SessionId::new(&binding.session_id);
    let session = state
        .session_store
        .session(&session_id)
        .ok_or_else(|| ApiError::session_not_found(session_id.as_str()))?;
    let current = match binding.workspace_id.as_deref() {
        Some(id) => {
            let directory = resolve_project_source_directory(
                state,
                &magi_core::WorkspaceId::new(id),
                Some(&session_id),
                binding.source_folder_id.as_deref(),
            )?;
            directory.execution_root
        }
        None if state.session_workspace_id(&session).is_none() => {
            state.personal_session_execution_root_path(&session_id)
        }
        None => return Err(ApiError::Conflict("终端原会话归属已变化".into())),
    };
    if current != binding.execution_root {
        return Err(ApiError::Conflict(
            "终端原执行目录已变化，请重新打开".into(),
        ));
    }
    Ok(())
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

async fn run_terminal_channel(
    socket: WebSocket,
    terminal: std::sync::Arc<TerminalSession>,
    state: ApiState,
) {
    let mut events = terminal.subscribe();
    let snapshot = terminal.snapshot();
    let snapshot_sequence = snapshot.sequence;
    let (mut sink, mut source) = socket.split();

    if !snapshot.output.is_empty()
        && sink
            .send(Message::Binary(snapshot.output.into()))
            .await
            .is_err()
    {
        return;
    }
    // 重连快照必须先重放历史输出，再发布当前生命周期。终态若先到达，
    // xterm 会先显示“进程已退出”再补旧输出，破坏真实时间顺序。
    if send_lifecycle(&mut sink, &snapshot.lifecycle)
        .await
        .is_err()
    {
        return;
    }
    if snapshot.lifecycle.is_terminal() {
        return;
    }
    let mut validation = tokio::time::interval(std::time::Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = validation.tick() => {
                if let Err(error) = validate_terminal_binding(&state, terminal.binding()) {
                    let _ = state.terminal_sessions.close(terminal.binding());
                    let _ = send_error(&mut sink, "terminal_directory_invalid", error.message()).await;
                    break;
                }
            }
            incoming = source.next() => {
                let Some(Ok(message)) = incoming else { break; };
                if let Err(error) = validate_terminal_binding(&state, terminal.binding()) {
                    let _ = state.terminal_sessions.close(terminal.binding());
                    let _ = send_error(&mut sink, "terminal_directory_invalid", error.message()).await;
                    break;
                }
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
    fn terminal_binding_keeps_source_identity_and_rejects_rebinding_and_revocation() {
        use super::super::test_project_fixtures::{add_source, set_members};
        use magi_core::{SessionId, WorkspaceId};
        use std::sync::Arc;
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let state = ApiState::new(
            "magi-test",
            Arc::new(magi_event_bus::InMemoryEventBus::new(32)),
            Arc::new(magi_session_store::SessionStore::default()),
            Arc::new(magi_workspace::WorkspaceStore::new()),
            Arc::new(magi_governance::GovernanceService::default()),
        );
        state
            .workspace_registry
            .register_native_path(WorkspaceId::new("terminal-project"), a.clone())
            .unwrap();
        let first = add_source(&state, "terminal-project", "second", &b).primary_source_folder_id;
        state
            .session_store
            .create_session_for_workspace(
                SessionId::new("terminal-session"),
                "terminal",
                Some("terminal-project".into()),
            )
            .unwrap();
        let mut query: TerminalScopeQuery = serde_json::from_value(serde_json::json!({
            "scope": "workspace", "sessionId": "terminal-session", "workspaceId": "terminal-project"
        }))
        .unwrap();
        assert!(resolve_terminal_binding(&state, "tab", &query).is_err());
        query.source_folder_id = Some(first.clone());
        let original = resolve_terminal_binding(&state, "tab", &query).unwrap();
        assert!(validate_terminal_binding(&state, &original).is_ok());
        assert_eq!(
            original.execution_root,
            magi_core::HostPath::canonicalize(&a)
                .unwrap()
                .into_path_buf()
        );
        state
            .terminal_sessions
            .open_or_create(original.clone(), PtySize::default())
            .unwrap();
        set_members(&state, "terminal-project", &[&first, "second"], "second");
        assert_eq!(
            resolve_terminal_binding(&state, "tab", &query).unwrap(),
            original
        );
        assert!(validate_terminal_binding(&state, &original).is_ok());
        query.source_folder_id = Some("second".into());
        let other = resolve_terminal_binding(&state, "tab", &query).unwrap();
        assert!(matches!(
            state
                .terminal_sessions
                .open_or_create(other, PtySize::default()),
            Err(TerminalRuntimeError::BindingMismatch)
        ));
        state.terminal_sessions.close(&original).unwrap();
        set_members(&state, "terminal-project", &["second"], "second");
        assert!(validate_terminal_binding(&state, &original).is_err());
        query.source_folder_id = Some(first);
        assert!(resolve_terminal_binding(&state, "tab", &query).is_err());
        query.source_folder_id = Some("second".into());
        std::fs::remove_dir_all(&b).unwrap();
        assert!(resolve_terminal_binding(&state, "tab", &query).is_err());
        assert!(!b.exists());
    }

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
