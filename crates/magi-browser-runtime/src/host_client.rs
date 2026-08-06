use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use magi_core::{BrowserCommandId, UtcMillis};
use sha2::{Digest, Sha256};
use tokio::{net::TcpStream, sync::broadcast};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
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

type HostWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type HostWebSocketSink = SplitSink<HostWebSocket, Message>;
type PendingResponse =
    tokio::sync::oneshot::Sender<Result<BrowserHostCommandReply, BrowserHostClientError>>;

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
    pub async fn connect(
        url: &str,
        auth_token: &str,
        handshake_timeout: Duration,
    ) -> Result<(Self, BrowserHostHandshake), BrowserHostClientError> {
        if auth_token.trim().is_empty() {
            return Err(BrowserHostClientError::InvalidConfiguration(
                "browser Host auth token cannot be empty".to_string(),
            ));
        }
        let mut request = url
            .into_client_request()
            .map_err(|error| BrowserHostClientError::Connect(error.to_string()))?;
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {auth_token}"))
                .map_err(|error| BrowserHostClientError::InvalidConfiguration(error.to_string()))?,
        );
        let (stream, _) = connect_async(request)
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
            request_timeout: Duration::from_secs(30),
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
        match tokio::time::timeout(self.request_timeout, receiver).await {
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
        BrowserHostEvent::ScreencastFrame(frame) => Some(BrowserHostBinaryPayload {
            payload_id: frame.payload_id.clone(),
            mime_type: frame.mime_type.clone(),
            byte_length: frame.byte_length,
            sha256: frame.sha256.clone(),
        }),
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
