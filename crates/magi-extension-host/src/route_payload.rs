//! 大路由响应的有界传输；句柄只属于当前宿主，不是可恢复的业务或文件存储。
use crate::{
    peer::Peer,
    protocol::{ErrorKind, RpcError},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_PAYLOAD_BYTES: usize = 24 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    id: String,
    bytes: usize,
    sha256: String,
    generation: u64,
}

struct Release<'a> {
    peer: &'a Peer,
    payload: Payload,
}
impl Drop for Release<'_> {
    fn drop(&mut self) {
        let _ = self.peer.notify(
            "route.result.release",
            json!({
                "id": self.payload.id, "generation": self.payload.generation,
            }),
        );
    }
}

pub(crate) async fn receive(peer: &Peer, generation: u64, value: Value) -> Result<Value, RpcError> {
    let Some(reference) = value.get("routePayload") else {
        return Ok(value);
    };
    let invalid = || RpcError::new(ErrorKind::ProtocolError, "路由响应传输身份或字节无效");
    if value.as_object().is_none_or(|object| object.len() != 1) {
        return Err(invalid());
    }
    let payload: Payload = serde_json::from_value(reference.clone()).map_err(|_| invalid())?;
    let release = Release { peer, payload };
    let payload = &release.payload;
    if payload.generation != generation
        || payload.bytes == 0
        || payload.bytes > MAX_PAYLOAD_BYTES
        || payload.id.len() != 36
        || !payload
            .id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        || payload.sha256.len() != 64
        || !payload.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(payload.bytes);
    while bytes.len() < payload.bytes {
        let part = peer
            .request(
                "route.result.read",
                json!({
                    "id": payload.id, "generation": generation, "offset": bytes.len(),
                }),
                Duration::from_secs(10),
            )
            .await?;
        if part.get("offset").and_then(Value::as_u64) != Some(bytes.len() as u64) {
            return Err(invalid());
        }
        let encoded = part
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if encoded.len() > CHUNK_BYTES.div_ceil(3) * 4 {
            return Err(invalid());
        }
        let chunk = STANDARD.decode(encoded).map_err(|_| invalid())?;
        if chunk.is_empty()
            || chunk.len() > CHUNK_BYTES
            || bytes.len() + chunk.len() > payload.bytes
            || STANDARD.encode(&chunk) != encoded
        {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk);
    }
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != payload.sha256 {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::{BoxFuture, Incoming, PeerLimits};
    use std::sync::{Arc, Mutex};

    struct PayloadSource {
        bytes: Vec<u8>,
        wrong_offset: bool,
        released: Arc<Mutex<Vec<Value>>>,
    }
    impl Incoming for PayloadSource {
        fn request(&self, _: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
            let offset = params["offset"].as_u64().unwrap() as usize;
            let data =
                STANDARD.encode(&self.bytes[offset..self.bytes.len().min(offset + CHUNK_BYTES)]);
            let offset = offset + usize::from(self.wrong_offset);
            Box::pin(async move { Ok(json!({"offset":offset,"data":data})) })
        }
        fn notification(&self, _: String, value: Value) {
            self.released.lock().unwrap().push(value);
        }
    }

    #[tokio::test]
    async fn payload_identity_digest_and_chunks_are_checked_and_always_released() {
        for corruption in ["none", "digest", "offset", "size", "generation"] {
            let bytes = serde_json::to_vec(&json!({"body":"完整🙂".repeat(40_000)})).unwrap();
            let released = Arc::default();
            let source = Arc::new(PayloadSource {
                bytes: bytes.clone(),
                wrong_offset: corruption == "offset",
                released: Arc::clone(&released),
            });
            let (a, b) = tokio::io::duplex(CHUNK_BYTES);
            let client = Peer::spawn(a, source.clone(), PeerLimits::default());
            let _server = Peer::spawn(b, source, PeerLimits::default());
            let reference = json!({"routePayload":{
                "id":"00000000-0000-0000-0000-000000000000", "bytes":if corruption == "size" { MAX_PAYLOAD_BYTES + 1 } else {bytes.len()},
                "sha256":if corruption == "digest" {"0".repeat(64)} else {Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect()},
                "generation":if corruption == "generation" {2} else {1},
            }});
            let result = receive(&client, 1, reference).await;
            assert_eq!(
                result.is_ok(),
                corruption == "none",
                "{corruption}: {result:?}"
            );
            if let Ok(result) = result {
                assert_eq!(result["body"], "完整🙂".repeat(40_000));
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while released.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(released.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn cancelling_a_chunk_read_releases_it_and_late_bytes_do_not_poison_the_next_response() {
        struct DelayedSource {
            bytes: Vec<u8>,
            started: tokio::sync::Notify,
            resume: tokio::sync::Notify,
            reads: std::sync::atomic::AtomicUsize,
            released: Mutex<Vec<Value>>,
        }
        impl Incoming for Arc<DelayedSource> {
            fn request(&self, _: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
                let source = Arc::clone(self);
                Box::pin(async move {
                    if source
                        .reads
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        == 0
                    {
                        source.started.notify_one();
                        source.resume.notified().await;
                    }
                    let offset = params["offset"].as_u64().unwrap() as usize;
                    Ok(json!({"offset":offset,"data":STANDARD.encode(
                        &source.bytes[offset..source.bytes.len().min(offset + CHUNK_BYTES)]
                    )}))
                })
            }
            fn notification(&self, method: String, value: Value) {
                assert_eq!(method, "route.result.release");
                self.released.lock().unwrap().push(value);
            }
        }
        let source = Arc::new(DelayedSource {
            bytes: serde_json::to_vec(&json!({"body":"完整🙂".repeat(40_000)})).unwrap(),
            started: Default::default(),
            resume: Default::default(),
            reads: Default::default(),
            released: Default::default(),
        });
        let (a, b) = tokio::io::duplex(CHUNK_BYTES);
        let handler = Arc::new(Arc::clone(&source));
        let client = Peer::spawn(a, handler.clone(), PeerLimits::default());
        let _server = Peer::spawn(b, handler, PeerLimits::default());
        let reference = json!({"routePayload":{
            "id":"00000000-0000-0000-0000-000000000000", "bytes":source.bytes.len(),
            "sha256":Sha256::digest(&source.bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>(), "generation":1,
        }});
        let receiving = {
            let peer = client.clone();
            let reference = reference.clone();
            tokio::spawn(async move { receive(&peer, 1, reference).await })
        };
        tokio::time::timeout(Duration::from_secs(1), source.started.notified())
            .await
            .unwrap();
        receiving.abort();
        assert!(receiving.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), async {
            while source.released.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            source.released.lock().unwrap()[0],
            json!({
                "id":reference["routePayload"]["id"], "generation":1,
            })
        );
        source.resume.notify_one();
        let next = receive(&client, 1, reference).await.unwrap();
        assert_eq!(next["body"], "完整🙂".repeat(40_000));
        assert!(!client.is_closed());
    }

    #[tokio::test]
    async fn node_payload_pool_cancels_only_its_owner_and_rejects_stale_generations() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("index.mjs");
        std::fs::write(&entry, r#"
export default async api => {
  let calls=0;
  await api.route('/resource',()=>({body:'x'.repeat(3*1024*1024+128*1024),headers:{'x-call':String(++calls)}}));
  await api.route('/count',()=>({body:calls}));
};
"#).unwrap();
        let node = std::env::var_os("MAGI_EXTENSION_NODE")
            .map(std::path::PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("test requires Node");
        let config = crate::instance::LaunchConfig::new(
            node,
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/host.mjs"),
            entry,
            dir.path().join("socket"),
        );
        let group = magi_process::ManagedProcessGroup::new();
        let instance = crate::instance::launch(&group, config, 7).await.unwrap();
        let invoke = |path, owner| {
            instance.peer.request(
                "route.invoke",
                json!({
                    "route":instance.resolve_route(path).unwrap(),"input":{"method":"GET","path":path},
                    "ctx":{"invocationId":owner,"generation":7},
                }),
                Duration::from_secs(5),
            )
        };
        let mut payloads = Vec::new();
        for index in 0..8 {
            let value = invoke("/resource", format!("owner-{index}")).await.unwrap();
            assert!(value["routePayload"]["id"].is_string(), "{value}");
            payloads.push(value);
        }
        let rejected = invoke("/resource", "over-capacity".into()).await.unwrap();
        assert_eq!(rejected["status"], 500);
        assert!(
            rejected["body"]["error"]
                .as_str()
                .unwrap()
                .contains("容量已满")
        );
        let first = &payloads[0]["routePayload"];
        let read = |id: Value, generation| {
            instance.peer.request(
                "route.result.read",
                json!({"id":id,"generation":generation,"offset":0}),
                Duration::from_secs(5),
            )
        };
        assert_eq!(
            read(first["id"].clone(), 6).await.unwrap_err().kind(),
            ErrorKind::StaleGeneration
        );
        instance
            .peer
            .notify(
                "route.result.release",
                json!({"id":first["id"],"generation":6}),
            )
            .unwrap();
        assert!(
            read(first["id"].clone(), 7).await.is_ok(),
            "旧代次不能释放当前载荷"
        );
        instance
            .peer
            .notify("host.cancel", json!({"invocationId":"owner-0"}))
            .unwrap();
        assert_eq!(
            read(first["id"].clone(), 7).await.unwrap_err().kind(),
            ErrorKind::BlobExpired
        );
        let replacement = invoke("/resource", "replacement".into()).await.unwrap();
        assert!(
            replacement["routePayload"]["id"].is_string(),
            "取消必须即时释放池名额"
        );
        let other = receive(&instance.peer, 7, payloads.remove(1))
            .await
            .unwrap();
        assert_eq!(other["headers"]["x-call"], "2");
        assert_eq!(
            other["body"].as_str().unwrap().len(),
            3 * 1024 * 1024 + 128 * 1024
        );
        let after_read = invoke("/resource", "after-read".into()).await.unwrap();
        assert!(
            after_read["routePayload"]["id"].is_string(),
            "完整交付释放池名额"
        );
        assert_eq!(
            invoke("/count", "count".into()).await.unwrap()["body"],
            11,
            "读取、取消和释放不能重放原业务；容量拒绝也必须保留实际执行计数"
        );
        instance.stop().await;
    }
}
