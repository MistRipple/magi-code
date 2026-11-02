//! 双向 JSON-RPC 对端：挂起请求表、有界发送队列、入站请求并发上限与连接关闭结算。

use crate::frame::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
use crate::protocol::{
    ErrorKind, Message, RpcError, notification, request, response_err, response_ok,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot, watch},
};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// 入站消息的处理者。
pub trait Incoming: Send + Sync + 'static {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>>;
    fn notification(&self, method: String, params: Value);
}

#[derive(Clone, Copy, Debug)]
pub struct PeerLimits {
    pub frame_bytes: usize,
    /// 每个方向的挂起请求上限。
    pub pending: usize,
    /// 发送队列容量。
    pub queue: usize,
}

impl Default for PeerLimits {
    fn default() -> Self {
        Self {
            frame_bytes: MAX_FRAME_BYTES,
            pending: 256,
            queue: 1024,
        }
    }
}

type Pending = Mutex<HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>;

struct Inner {
    out: mpsc::Sender<Value>,
    pending: Pending,
    next_id: AtomicU64,
    incoming_inflight: AtomicU64,
    closed: watch::Sender<bool>,
    limits: PeerLimits,
}

#[derive(Clone)]
pub struct Peer {
    inner: Arc<Inner>,
}

impl Peer {
    pub fn spawn<S>(stream: S, handler: Arc<dyn Incoming>, limits: PeerLimits) -> Peer
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (out_tx, mut out_rx) = mpsc::channel::<Value>(limits.queue);
        let (closed_tx, closed_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            out: out_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            incoming_inflight: AtomicU64::new(0),
            closed: closed_tx,
            limits,
        });
        let (mut reader, mut writer) = tokio::io::split(stream);

        let writer_inner = Arc::clone(&inner);
        let mut writer_closed = closed_rx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    message = out_rx.recv() => {
                        let Some(message) = message else { break };
                        if write_frame(&mut writer, &message, writer_inner.limits.frame_bytes).await.is_err() {
                            break;
                        }
                    }
                    _ = writer_closed.changed() => break,
                }
            }
            writer_inner.close();
        });

        let reader_inner = Arc::clone(&inner);
        let mut reader_closed = closed_rx;
        tokio::spawn(async move {
            loop {
                let frame = tokio::select! {
                    frame = read_frame(&mut reader, reader_inner.limits.frame_bytes) => frame,
                    _ = reader_closed.changed() => break,
                };
                match frame {
                    Ok(Some(value)) => match Message::parse(value) {
                        Ok(message) => reader_inner.dispatch(message, &handler),
                        Err(error) => {
                            tracing::warn!(%error, "扩展对端协议违规，断开连接");
                            break;
                        }
                    },
                    Ok(None) => break,
                    Err(FrameError::TooLarge { .. } | FrameError::InvalidJson) => {
                        tracing::warn!("扩展对端帧违规，断开连接");
                        break;
                    }
                    Err(FrameError::Io(_)) => break,
                }
            }
            reader_inner.close();
        });

        Peer { inner }
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, RpcError> {
        if *self.inner.closed.borrow() {
            return Err(RpcError::new(ErrorKind::HostGone, "连接已关闭"));
        }
        let id = format!("d{}", self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().expect("pending lock poisoned");
            if pending.len() >= self.inner.limits.pending {
                return Err(RpcError::new(ErrorKind::Backpressure, "挂起请求过多"));
            }
            pending.insert(id.clone(), tx);
        }
        if self
            .inner
            .out
            .try_send(request(&id, method, params))
            .is_err()
        {
            self.inner.take_pending(&id);
            return Err(RpcError::new(
                ErrorKind::Backpressure,
                "发送队列已满或连接已关闭",
            ));
        }
        // 调用方丢弃本 future（如被取消）时同样释放挂起槽位，避免占满 `pending` 上限。
        struct PendingGuard<'a>(&'a Inner, &'a str);
        impl Drop for PendingGuard<'_> {
            fn drop(&mut self) {
                self.0.take_pending(self.1);
            }
        }
        let _guard = PendingGuard(&self.inner, &id);
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::new(ErrorKind::HostGone, "连接已关闭")),
            Err(_) => {
                self.inner.take_pending(&id);
                Err(RpcError::new(
                    ErrorKind::DeadlineExceeded,
                    format!("{method} 超时"),
                ))
            }
        }
    }

    /// 发送通知。队列满返回 `Backpressure`，不阻塞调用方。
    pub fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        if *self.inner.closed.borrow() {
            return Err(RpcError::new(ErrorKind::HostGone, "连接已关闭"));
        }
        self.inner
            .out
            .try_send(notification(method, params))
            .map_err(|_| RpcError::new(ErrorKind::Backpressure, "发送队列已满"))
    }

    pub fn is_closed(&self) -> bool {
        *self.inner.closed.borrow()
    }

    /// 等待连接关闭。
    pub async fn closed(&self) {
        let mut rx = self.inner.closed.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn close(&self) {
        self.inner.close();
    }
}

impl Inner {
    fn take_pending(&self, id: &str) -> Option<oneshot::Sender<Result<Value, RpcError>>> {
        self.pending
            .lock()
            .expect("pending lock poisoned")
            .remove(id)
    }

    /// 关闭并结算：所有挂起请求以 `HostGone` 结束。
    fn close(&self) {
        self.closed.send_replace(true);
        let drained: Vec<_> = self
            .pending
            .lock()
            .expect("pending lock poisoned")
            .drain()
            .collect();
        for (_, tx) in drained {
            let _ = tx.send(Err(RpcError::new(ErrorKind::HostGone, "连接已关闭")));
        }
    }

    fn dispatch(self: &Arc<Self>, message: Message, handler: &Arc<dyn Incoming>) {
        match message {
            Message::Response { id, result } => {
                // 迟到响应（请求已超时被移除）直接丢弃。
                if let Some(tx) = self.take_pending(&id) {
                    let _ = tx.send(result);
                }
            }
            Message::Notification { method, params } => handler.notification(method, params),
            Message::Request { id, method, params } => {
                if self.incoming_inflight.fetch_add(1, Ordering::SeqCst)
                    >= self.limits.pending as u64
                {
                    self.incoming_inflight.fetch_sub(1, Ordering::SeqCst);
                    let _ = self.out.try_send(response_err(
                        &id,
                        &RpcError::new(ErrorKind::Backpressure, "入站请求过多"),
                    ));
                    return;
                }
                let inner = Arc::clone(self);
                let handler = Arc::clone(handler);
                tokio::spawn(async move {
                    let reply = match handler.request(method, params).await {
                        Ok(result) => response_ok(&id, result),
                        Err(error) => response_err(&id, &error),
                    };
                    inner.incoming_inflight.fetch_sub(1, Ordering::SeqCst);
                    let _ = inner.out.send(reply).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Echo;
    impl Incoming for Echo {
        fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
            Box::pin(async move {
                match method.as_str() {
                    "echo" => Ok(params),
                    "slow" => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        Ok(Value::Null)
                    }
                    _ => Err(RpcError::new(ErrorKind::MethodNotFound, method)),
                }
            })
        }
        fn notification(&self, _: String, _: Value) {}
    }

    fn pair() -> (Peer, Peer) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        (
            Peer::spawn(a, Arc::new(Echo), PeerLimits::default()),
            Peer::spawn(b, Arc::new(Echo), PeerLimits::default()),
        )
    }

    #[tokio::test]
    async fn requests_get_results_errors_and_deadlines() {
        let (a, _b) = pair();
        assert_eq!(
            a.request("echo", json!({"x": 1}), Duration::from_secs(2))
                .await
                .unwrap(),
            json!({"x": 1})
        );
        let error = a
            .request("nope", json!({}), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MethodNotFound);
        let error = a
            .request("slow", json!({}), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DeadlineExceeded);
    }

    #[tokio::test]
    async fn closing_settles_pending_requests_as_host_gone() {
        let (a, b) = pair();
        let pending = {
            let a = a.clone();
            tokio::spawn(async move { a.request("slow", json!({}), Duration::from_secs(10)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        b.close();
        let error = pending.await.unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::HostGone);
        a.closed().await;
        assert_eq!(
            a.request("echo", json!({}), Duration::from_secs(1))
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::HostGone
        );
    }

    #[tokio::test]
    async fn pending_requests_are_bounded() {
        let (x, y) = tokio::io::duplex(64 * 1024);
        let limits = PeerLimits {
            pending: 2,
            ..PeerLimits::default()
        };
        let a = Peer::spawn(x, Arc::new(Echo), limits);
        let _b = Peer::spawn(y, Arc::new(Echo), PeerLimits::default());
        let slow = |a: Peer| {
            tokio::spawn(async move { a.request("slow", json!({}), Duration::from_secs(5)).await })
        };
        let first = slow(a.clone());
        let second = slow(a.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let error = a
            .request("echo", json!({}), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Backpressure);
        first.abort();
        second.abort();
    }
}
