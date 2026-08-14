use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[cfg(unix)]
use std::path::Path;

use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use magi_core::{BrowserCommandId, UtcMillis};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::broadcast,
};
use tokio_tungstenite::{
    WebSocketStream, client_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

use crate::{
    BrowserHostBinaryPayload, BrowserHostCommand, BrowserHostCommandOutcome,
    BrowserHostCommandResult, BrowserHostEvent, BrowserHostEventEnvelope, BrowserHostHandshake,
    BrowserHostProtocolVersion, BrowserHostRequestEnvelope, BrowserHostResponseEnvelope,
};

trait DesktopControlStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> DesktopControlStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

type HostWebSocket = WebSocketStream<Box<dyn DesktopControlStream>>;
type HostWebSocketSink = SplitSink<HostWebSocket, Message>;
type PendingResponse =
    tokio::sync::oneshot::Sender<Result<BrowserHostCommandReply, BrowserHostClientError>>;

const DEFAULT_BROWSER_HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
// 页面导航自身允许使用 60 秒，并且 Host 还需要在导航失败后停止加载、
// 收口画面与页面状态。客户端必须给这些收口步骤留出独立余量，否则
// 一个可恢复的 navigation timeout 会被错误改写成 Host disconnected。
const NAVIGATION_BROWSER_HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const LONG_RUNNING_BROWSER_HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct BrowserHostCommandReply {
    pub response: BrowserHostResponseEnvelope,
    pub binary: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct BrowserHostIncomingEvent {
    pub envelope: BrowserHostEventEnvelope,
    pub binary: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct BrowserHostClient {
    sink: Arc<tokio::sync::Mutex<HostWebSocketSink>>,
    pending: Arc<Mutex<HashMap<BrowserCommandId, PendingResponse>>>,
    events: broadcast::Sender<BrowserHostIncomingEvent>,
    command_sequence: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    request_timeout: Duration,
}

impl BrowserHostClient {
    pub async fn connect_desktop_socket(
        socket_path: &str,
        auth_token: &str,
        expected_desktop_epoch: &str,
        expected_process_id: u32,
        handshake_timeout: Duration,
    ) -> Result<(Self, BrowserHostHandshake), BrowserHostClientError> {
        if socket_path.trim().is_empty() {
            return Err(BrowserHostClientError::InvalidConfiguration(
                "Desktop control socket path cannot be empty".to_string(),
            ));
        }
        if auth_token.trim().is_empty() {
            return Err(BrowserHostClientError::InvalidConfiguration(
                "Desktop control auth token cannot be empty".to_string(),
            ));
        }
        if expected_desktop_epoch.trim().is_empty() {
            return Err(BrowserHostClientError::InvalidConfiguration(
                "expected Desktop epoch cannot be empty".to_string(),
            ));
        }
        if expected_process_id == 0 {
            return Err(BrowserHostClientError::InvalidConfiguration(
                "expected Desktop process id cannot be zero".to_string(),
            ));
        }
        let mut request = "ws://magi.desktop/control"
            .into_client_request()
            .map_err(|error| BrowserHostClientError::Connect(error.to_string()))?;
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {auth_token}"))
                .map_err(|error| BrowserHostClientError::InvalidConfiguration(error.to_string()))?,
        );
        let transport = connect_desktop_transport(socket_path).await?;
        let (stream, _) = client_async(request, transport)
            .await
            .map_err(|error| BrowserHostClientError::Connect(error.to_string()))?;
        let (sink, source) = stream.split();
        let (events, _) = broadcast::channel(256);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (handshake_sender, handshake_receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(read_host_messages(
            source,
            Arc::clone(&pending),
            events.clone(),
            Arc::clone(&closed),
            Some(handshake_sender),
        ));
        let client = Self {
            sink: Arc::new(tokio::sync::Mutex::new(sink)),
            pending,
            events,
            command_sequence: Arc::new(AtomicU64::new(1)),
            closed,
            request_timeout: DEFAULT_BROWSER_HOST_REQUEST_TIMEOUT,
        };
        let handshake = tokio::time::timeout(handshake_timeout, handshake_receiver)
            .await
            .map_err(|_| BrowserHostClientError::HandshakeTimeout)?
            .map_err(|_| BrowserHostClientError::Disconnected)??;
        if handshake.protocol_version != BrowserHostProtocolVersion::CURRENT {
            client.close().await;
            return Err(BrowserHostClientError::ProtocolIncompatible {
                expected: BrowserHostProtocolVersion::CURRENT,
                received: handshake.protocol_version,
            });
        }
        if let Err(error) =
            validate_desktop_identity(&handshake, expected_desktop_epoch, expected_process_id)
        {
            client.close().await;
            return Err(error);
        }
        Ok((client, handshake))
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BrowserHostIncomingEvent> {
        self.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub async fn request(
        &self,
        command: BrowserHostCommand,
    ) -> Result<BrowserHostCommandReply, BrowserHostClientError> {
        if self.is_closed() {
            return Err(BrowserHostClientError::Disconnected);
        }
        let request_id = BrowserCommandId::new(format!(
            "browser-command-{}-{}",
            UtcMillis::now().0,
            self.command_sequence.fetch_add(1, Ordering::Relaxed)
        ));
        let request_timeout = request_timeout_for(&command, self.request_timeout);
        let envelope = BrowserHostRequestEnvelope {
            request_id: request_id.clone(),
            protocol_version: BrowserHostProtocolVersion::CURRENT,
            command,
        };
        let payload = serde_json::to_string(&envelope)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .expect("browser Host pending response lock poisoned")
            .insert(request_id.clone(), sender);
        let send_result = self
            .sink
            .lock()
            .await
            .send(Message::Text(payload.into()))
            .await;
        if let Err(error) = send_result {
            self.pending
                .lock()
                .expect("browser Host pending response lock poisoned")
                .remove(&request_id);
            return Err(BrowserHostClientError::Transport(error.to_string()));
        }
        match tokio::time::timeout(request_timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BrowserHostClientError::Disconnected),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("browser Host pending response lock poisoned")
                    .remove(&request_id);
                Err(BrowserHostClientError::RequestTimeout(request_id))
            }
        }
    }

    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.sink.lock().await.send(Message::Close(None)).await;
        fail_pending(&self.pending, BrowserHostClientError::Disconnected);
    }
}

#[cfg(unix)]
async fn connect_desktop_transport(
    socket_path: &str,
) -> Result<Box<dyn DesktopControlStream>, BrowserHostClientError> {
    let stream = tokio::net::UnixStream::connect(Path::new(socket_path))
        .await
        .map_err(|error| BrowserHostClientError::Connect(error.to_string()))?;
    Ok(Box::new(stream))
}

#[cfg(windows)]
async fn connect_desktop_transport(
    socket_path: &str,
) -> Result<Box<dyn DesktopControlStream>, BrowserHostClientError> {
    let stream = tokio::net::windows::named_pipe::ClientOptions::new()
        .open(socket_path)
        .map_err(|error| BrowserHostClientError::Connect(error.to_string()))?;
    Ok(Box::new(stream))
}

fn validate_desktop_identity(
    handshake: &BrowserHostHandshake,
    expected_desktop_epoch: &str,
    expected_process_id: u32,
) -> Result<(), BrowserHostClientError> {
    if handshake.desktop_epoch != expected_desktop_epoch {
        return Err(BrowserHostClientError::DesktopEpochMismatch {
            expected: expected_desktop_epoch.to_string(),
            received: handshake.desktop_epoch.clone(),
        });
    }
    if handshake.process_id != expected_process_id {
        return Err(BrowserHostClientError::DesktopProcessMismatch {
            expected: expected_process_id,
            received: handshake.process_id,
        });
    }
    Ok(())
}

fn request_timeout_for(command: &BrowserHostCommand, default: Duration) -> Duration {
    if matches!(command, BrowserHostCommand::Navigate { .. }) {
        return default.max(NAVIGATION_BROWSER_HOST_REQUEST_TIMEOUT);
    }
    if matches!(
        command,
        BrowserHostCommand::Devtools { operation, .. }
            if matches!(operation.as_str(), "lighthouse" | "heap")
    ) {
        return default.max(LONG_RUNNING_BROWSER_HOST_REQUEST_TIMEOUT);
    }
    default
}

async fn read_host_messages(
    mut source: futures_util::stream::SplitStream<HostWebSocket>,
    pending: Arc<Mutex<HashMap<BrowserCommandId, PendingResponse>>>,
    events: broadcast::Sender<BrowserHostIncomingEvent>,
    closed: Arc<AtomicBool>,
    mut handshake_sender: Option<
        tokio::sync::oneshot::Sender<Result<BrowserHostHandshake, BrowserHostClientError>>,
    >,
) {
    let mut binary_queue = VecDeque::<PendingBinary>::new();
    while let Some(message) = source.next().await {
        let result = match message {
            Ok(Message::Text(text)) => handle_text_message(
                text.as_str(),
                &pending,
                &events,
                &mut binary_queue,
                &mut handshake_sender,
            ),
            Ok(Message::Binary(bytes)) => {
                handle_binary_message(bytes.to_vec(), &events, &mut binary_queue)
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => Ok(()),
            Ok(Message::Close(_)) => break,
            Ok(Message::Frame(_)) => Ok(()),
            Err(error) => Err(BrowserHostClientError::Transport(error.to_string())),
        };
        if let Err(error) = result {
            if let Some(sender) = handshake_sender.take() {
                let _ = sender.send(Err(error.clone()));
            }
            fail_pending(&pending, error);
            break;
        }
    }
    closed.store(true, Ordering::Release);
    if let Some(sender) = handshake_sender.take() {
        let _ = sender.send(Err(BrowserHostClientError::Disconnected));
    }
    fail_pending(&pending, BrowserHostClientError::Disconnected);
}

fn handle_text_message(
    text: &str,
    pending: &Arc<Mutex<HashMap<BrowserCommandId, PendingResponse>>>,
    events: &broadcast::Sender<BrowserHostIncomingEvent>,
    binary_queue: &mut VecDeque<PendingBinary>,
    handshake_sender: &mut Option<
        tokio::sync::oneshot::Sender<Result<BrowserHostHandshake, BrowserHostClientError>>,
    >,
) -> Result<(), BrowserHostClientError> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    if value.get("request_id").is_some() {
        let response: BrowserHostResponseEnvelope = serde_json::from_value(value)?;
        ensure_protocol(response.protocol_version)?;
        let sender = pending
            .lock()
            .expect("browser Host pending response lock poisoned")
            .remove(&response.request_id);
        let Some(sender) = sender else {
            return Ok(());
        };
        if let Some(metadata) = response_binary_metadata(&response) {
            binary_queue.push_back(PendingBinary::Response {
                response,
                sender,
                metadata,
            });
        } else {
            let _ = sender.send(Ok(BrowserHostCommandReply {
                response,
                binary: None,
            }));
        }
        return Ok(());
    }
    let envelope: BrowserHostEventEnvelope = serde_json::from_value(value)?;
    ensure_protocol(envelope.protocol_version)?;
    if let BrowserHostEvent::Ready(handshake) = &envelope.event
        && let Some(sender) = handshake_sender.take()
    {
        let _ = sender.send(Ok(handshake.clone()));
    }
    if let Some(metadata) = event_binary_metadata(&envelope) {
        binary_queue.push_back(PendingBinary::Event { envelope, metadata });
    } else {
        let _ = events.send(BrowserHostIncomingEvent {
            envelope,
            binary: None,
        });
    }
    Ok(())
}

fn handle_binary_message(
    bytes: Vec<u8>,
    events: &broadcast::Sender<BrowserHostIncomingEvent>,
    binary_queue: &mut VecDeque<PendingBinary>,
) -> Result<(), BrowserHostClientError> {
    let pending = binary_queue
        .pop_front()
        .ok_or(BrowserHostClientError::UnexpectedBinaryPayload)?;
    let metadata = pending.metadata();
    verify_binary(&bytes, metadata)?;
    match pending {
        PendingBinary::Response {
            response, sender, ..
        } => {
            let _ = sender.send(Ok(BrowserHostCommandReply {
                response,
                binary: Some(bytes),
            }));
        }
        PendingBinary::Event { envelope, .. } => {
            let _ = events.send(BrowserHostIncomingEvent {
                envelope,
                binary: Some(bytes),
            });
        }
    }
    Ok(())
}

enum PendingBinary {
    Response {
        response: BrowserHostResponseEnvelope,
        sender: PendingResponse,
        metadata: BrowserHostBinaryPayload,
    },
    Event {
        envelope: BrowserHostEventEnvelope,
        metadata: BrowserHostBinaryPayload,
    },
}

impl PendingBinary {
    fn metadata(&self) -> &BrowserHostBinaryPayload {
        match self {
            Self::Response { metadata, .. } | Self::Event { metadata, .. } => metadata,
        }
    }
}

fn response_binary_metadata(
    response: &BrowserHostResponseEnvelope,
) -> Option<BrowserHostBinaryPayload> {
    match &response.outcome {
        BrowserHostCommandOutcome::Succeeded(result) => match result.as_ref() {
            BrowserHostCommandResult::BinaryPayload(metadata) => Some(metadata.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn event_binary_metadata(envelope: &BrowserHostEventEnvelope) -> Option<BrowserHostBinaryPayload> {
    match &envelope.event {
        BrowserHostEvent::BinaryPayloadReady(metadata) => Some(metadata.clone()),
        _ => None,
    }
}

fn verify_binary(
    bytes: &[u8],
    metadata: &BrowserHostBinaryPayload,
) -> Result<(), BrowserHostClientError> {
    if bytes.len() as u64 != metadata.byte_length {
        return Err(BrowserHostClientError::BinarySizeMismatch {
            expected: metadata.byte_length,
            received: bytes.len() as u64,
        });
    }
    if !metadata.sha256.is_empty() {
        let actual = format!("{:x}", Sha256::digest(bytes));
        if actual != metadata.sha256 {
            return Err(BrowserHostClientError::BinaryHashMismatch);
        }
    }
    Ok(())
}

fn ensure_protocol(received: BrowserHostProtocolVersion) -> Result<(), BrowserHostClientError> {
    if received != BrowserHostProtocolVersion::CURRENT {
        return Err(BrowserHostClientError::ProtocolIncompatible {
            expected: BrowserHostProtocolVersion::CURRENT,
            received,
        });
    }
    Ok(())
}

fn fail_pending(
    pending: &Arc<Mutex<HashMap<BrowserCommandId, PendingResponse>>>,
    error: BrowserHostClientError,
) {
    let pending = std::mem::take(
        &mut *pending
            .lock()
            .expect("browser Host pending response lock poisoned"),
    );
    for (_, sender) in pending {
        let _ = sender.send(Err(error.clone()));
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum BrowserHostClientError {
    #[error("browser Host client configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("browser Host connection failed: {0}")]
    Connect(String),
    #[error("browser Host transport failed: {0}")]
    Transport(String),
    #[error("browser Host JSON protocol failed: {0}")]
    Json(String),
    #[error("browser Host handshake timed out")]
    HandshakeTimeout,
    #[error("browser Host disconnected")]
    Disconnected,
    #[error("browser Host protocol is incompatible: expected={expected:?}, received={received:?}")]
    ProtocolIncompatible {
        expected: BrowserHostProtocolVersion,
        received: BrowserHostProtocolVersion,
    },
    #[error("Desktop epoch mismatch: expected={expected}, received={received}")]
    DesktopEpochMismatch { expected: String, received: String },
    #[error("Desktop process mismatch: expected={expected}, received={received}")]
    DesktopProcessMismatch { expected: u32, received: u32 },
    #[error("browser Host request timed out: {0}")]
    RequestTimeout(BrowserCommandId),
    #[error("browser Host sent an unexpected binary payload")]
    UnexpectedBinaryPayload,
    #[error("browser Host binary size mismatch: expected={expected}, received={received}")]
    BinarySizeMismatch { expected: u64, received: u64 },
    #[error("browser Host binary SHA-256 mismatch")]
    BinaryHashMismatch,
}

impl From<serde_json::Error> for BrowserHostClientError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> BrowserHostHandshake {
        BrowserHostHandshake {
            protocol_version: BrowserHostProtocolVersion::CURRENT,
            desktop_version: "desktop-test".to_string(),
            electron_version: "electron-test".to_string(),
            chromium_version: "chromium-test".to_string(),
            process_id: 42,
            desktop_epoch: "desktop-epoch".to_string(),
            worker_epoch: "worker-epoch".to_string(),
        }
    }

    fn devtools(operation: &str) -> BrowserHostCommand {
        BrowserHostCommand::Devtools {
            tab_id: magi_core::BrowserTabId::new("test-tab"),
            control: None,
            operation: operation.to_string(),
            arguments: serde_json::Value::Null,
        }
    }

    #[test]
    fn long_running_devtools_operations_have_a_dedicated_timeout() {
        assert_eq!(
            request_timeout_for(&devtools("lighthouse"), Duration::from_secs(30)),
            Duration::from_secs(120)
        );
        assert_eq!(
            request_timeout_for(&devtools("heap"), Duration::from_secs(30)),
            Duration::from_secs(120)
        );
        assert_eq!(
            request_timeout_for(&devtools("performance"), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
        assert_eq!(
            request_timeout_for(&devtools("lighthouse"), Duration::from_secs(180)),
            Duration::from_secs(180)
        );
    }

    #[test]
    fn navigation_requests_have_time_to_report_a_page_timeout() {
        let navigation = BrowserHostCommand::Navigate {
            tab_id: magi_core::BrowserTabId::new("test-tab"),
            control: crate::BrowserHostControl::User { fence: 1 },
            navigation: crate::BrowserNavigation::Url {
                url: "https://example.com".to_string(),
                handle_before_unload: None,
                init_script: None,
                timeout_ms: Some(60_000),
            },
        };
        assert_eq!(
            request_timeout_for(&navigation, Duration::from_secs(30)),
            Duration::from_secs(120)
        );
        assert_eq!(
            request_timeout_for(&navigation, Duration::from_secs(180)),
            Duration::from_secs(180)
        );
    }

    #[test]
    fn desktop_identity_must_match_the_supervised_process() {
        assert!(validate_desktop_identity(&handshake(), "desktop-epoch", 42).is_ok());
        assert!(matches!(
            validate_desktop_identity(&handshake(), "other-epoch", 42),
            Err(BrowserHostClientError::DesktopEpochMismatch { .. })
        ));
        assert!(matches!(
            validate_desktop_identity(&handshake(), "desktop-epoch", 7),
            Err(BrowserHostClientError::DesktopProcessMismatch { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn connects_to_desktop_over_unix_socket_websocket() {
        use tokio::net::UnixListener;
        use tokio_tungstenite::accept_hdr_async;

        let temp_dir = tempfile::tempdir().expect("create temporary socket directory");
        let socket_path = temp_dir.path().join("desktop-control.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind Desktop control socket");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept Desktop client");
            let mut websocket = accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().path(), "/control");
                    assert_eq!(
                        request.headers().get(AUTHORIZATION),
                        Some(&HeaderValue::from_static("Bearer test-token"))
                    );
                    Ok(response)
                },
            )
            .await
            .expect("upgrade Desktop control websocket");
            let ready = BrowserHostEventEnvelope {
                protocol_version: BrowserHostProtocolVersion::CURRENT,
                sequence: 1,
                event: BrowserHostEvent::Ready(handshake()),
            };
            websocket
                .send(Message::Text(
                    serde_json::to_string(&ready)
                        .expect("serialize ready event")
                        .into(),
                ))
                .await
                .expect("send ready event");

            let request = websocket
                .next()
                .await
                .expect("receive ping request")
                .expect("read ping request");
            let Message::Text(request) = request else {
                panic!("expected text request");
            };
            let request: BrowserHostRequestEnvelope =
                serde_json::from_str(request.as_str()).expect("decode ping request");
            assert_eq!(request.command, BrowserHostCommand::Ping);
            let response = BrowserHostResponseEnvelope {
                request_id: request.request_id,
                protocol_version: BrowserHostProtocolVersion::CURRENT,
                outcome: BrowserHostCommandOutcome::Succeeded(Box::new(
                    BrowserHostCommandResult::Pong {
                        monotonic_millis: 123,
                    },
                )),
            };
            websocket
                .send(Message::Text(
                    serde_json::to_string(&response)
                        .expect("serialize ping response")
                        .into(),
                ))
                .await
                .expect("send ping response");
        });

        let (client, received_handshake) = BrowserHostClient::connect_desktop_socket(
            socket_path.to_str().expect("UTF-8 socket path"),
            "test-token",
            "desktop-epoch",
            42,
            Duration::from_secs(2),
        )
        .await
        .expect("connect Desktop client");
        assert_eq!(received_handshake, handshake());
        let reply = client
            .request(BrowserHostCommand::Ping)
            .await
            .expect("request ping");
        assert!(matches!(
            reply.response.outcome,
            BrowserHostCommandOutcome::Succeeded(result)
                if matches!(*result, BrowserHostCommandResult::Pong { monotonic_millis: 123 })
        ));
        server.await.expect("join Desktop control server");
    }
}
