//! 单个宿主实例：启动 Node 进程、握手、注册裁决与停止。

use crate::frame::{read_frame, write_frame};
use crate::peer::{BoxFuture, Incoming, Peer, PeerLimits};
use crate::protocol::{
    ErrorKind, HookMode, HookRegistration, Message, PROTOCOL_MAJOR, PROTOCOL_MINOR, RpcError,
    Version, response_err, response_ok,
};
use crate::transport;
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Notify, watch},
};

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    /// 产品随包的固定版本 Node 可执行文件。
    pub node: PathBuf,
    /// 宿主运行时脚本（`runtime/host.mjs`）。
    pub runtime_entry: PathBuf,
    /// 插件入口（包内 `extension/index.mjs`）。
    pub plugin_entry: PathBuf,
    /// 套接字目录（Unix）；Windows 忽略。
    pub endpoint_dir: PathBuf,
    pub plugin_id: String,
    pub version: String,
    pub digest: String,
    pub grants: Vec<String>,
    /// 清单声明并获批的钩子。
    pub hooks: Vec<HookRegistration>,
    pub config: Value,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub ready_timeout: Duration,
    pub limits: PeerLimits,
}

impl LaunchConfig {
    pub fn new(
        node: PathBuf,
        runtime_entry: PathBuf,
        plugin_entry: PathBuf,
        endpoint_dir: PathBuf,
    ) -> Self {
        Self {
            node,
            runtime_entry,
            plugin_entry,
            endpoint_dir,
            plugin_id: String::new(),
            version: String::new(),
            digest: String::new(),
            grants: Vec::new(),
            hooks: Vec::new(),
            config: Value::Null,
            connect_timeout: Duration::from_secs(15),
            handshake_timeout: Duration::from_secs(5),
            ready_timeout: Duration::from_secs(10),
            limits: PeerLimits::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("启动 Node 失败: {0}")]
    Spawn(std::io::Error),
    #[error("端点错误: {0}")]
    Endpoint(std::io::Error),
    #[error("宿主未在期限内连接{0}")]
    ConnectTimeout(String),
    #[error("宿主在连接前已退出{0}")]
    ExitedBeforeConnect(String),
    #[error("握手失败: {0}")]
    Handshake(String),
    #[error("扩展激活失败或超时{0}")]
    NotReady(String),
    #[error("钩子声明无效: {0}")]
    InvalidHook(RpcError),
}

/// 一个获批钩子及其生效 deadline。
#[derive(Clone, Copy, Debug)]
pub struct ApprovedHook {
    pub mode: HookMode,
    pub deadline: Duration,
}

pub struct HostInstance {
    pub plugin_id: String,
    pub version: String,
    /// 插件包摘要；提示贡献的片段身份包含它。
    pub digest: String,
    pub generation: u64,
    pub(crate) peer: Peer,
    approved: HashMap<String, ApprovedHook>,
    bound: Arc<Mutex<HashSet<String>>>,
    stop: Arc<Notify>,
    exited: watch::Receiver<bool>,
}

impl HostInstance {
    pub fn approved(&self, hook: &str) -> Option<ApprovedHook> {
        self.approved.get(hook).copied()
    }

    /// 宿主已通过 `register.hook` 绑定处理器的钩子才会收到分发。
    pub fn is_bound(&self, hook: &str) -> bool {
        self.bound
            .lock()
            .expect("bound lock poisoned")
            .contains(hook)
    }

    pub fn is_alive(&self) -> bool {
        !*self.exited.borrow() && !self.peer.is_closed()
    }

    /// 连接关闭或进程退出时返回。
    pub async fn wait_exit(&self) {
        let mut exited = self.exited.clone();
        tokio::select! {
            _ = async {
                while !*exited.borrow_and_update() {
                    if exited.changed().await.is_err() { break; }
                }
            } => {}
            _ = self.peer.closed() => {}
        }
    }

    /// 请求有限排空后终止整个受管进程树。
    pub async fn stop(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.peer
                .request("host.shutdown", json!({}), Duration::from_secs(2)),
        )
        .await;
        self.stop.notify_one();
        let mut exited = self.exited.clone();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while !*exited.borrow_and_update() {
                if exited.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
        self.peer.close();
    }
}

struct HostHandler {
    plugin_id: String,
    approved: HashMap<String, ApprovedHook>,
    bound: Arc<Mutex<HashSet<String>>>,
    ready: watch::Sender<bool>,
}

impl Incoming for HostHandler {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        let result = match method.as_str() {
            "register.hook" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.approved.contains_key(name) {
                    self.bound
                        .lock()
                        .expect("bound lock poisoned")
                        .insert(name.to_string());
                    Ok(json!({}))
                } else {
                    Err(RpcError::new(
                        ErrorKind::RegistrationDenied,
                        format!("钩子 {name} 未在清单中声明或未获批准"),
                    ))
                }
            }
            _ => Err(RpcError::new(ErrorKind::MethodNotFound, method)),
        };
        Box::pin(async move { result })
    }

    fn notification(&self, method: String, params: Value) {
        match method.as_str() {
            "host.ready" => {
                self.ready.send_replace(true);
            }
            "host.log" => {
                let message = params.get("message").and_then(Value::as_str).unwrap_or("");
                tracing::debug!(plugin = %self.plugin_id, %message, "扩展日志");
            }
            _ => {}
        }
    }
}

fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(buffer.iter().map(|b| format!("{b:02x}")).collect())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub async fn launch(
    group: &ManagedProcessGroup,
    config: LaunchConfig,
    generation: u64,
) -> Result<Arc<HostInstance>, LaunchError> {
    let mut approved = HashMap::new();
    for hook in &config.hooks {
        let deadline = hook.validate().map_err(LaunchError::InvalidHook)?;
        approved.insert(
            hook.name.clone(),
            ApprovedHook {
                mode: hook.mode,
                deadline,
            },
        );
    }

    let endpoint = transport::new_endpoint(&config.endpoint_dir, &format!("g{generation}"))
        .map_err(LaunchError::Endpoint)?;
    let mut listener = transport::bind(&endpoint).map_err(LaunchError::Endpoint)?;
    let token = random_hex(32).map_err(LaunchError::Endpoint)?;

    let mut command = magi_process::tokio_command(&config.node);
    command
        .arg(&config.runtime_entry)
        .env("MAGI_EXT_ENDPOINT", &endpoint)
        .env("MAGI_EXT_TOKEN", &token)
        .env("MAGI_EXT_ENTRY", &config.plugin_entry)
        .current_dir(config.plugin_entry.parent().unwrap_or(&config.endpoint_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = group
        .spawn_tokio(&mut command)
        .map_err(LaunchError::Spawn)?;

    // 保留最近的 stderr，仅用于失败诊断；不进入 canonical 事实。
    let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
    if let Some(stderr) = child.take_stderr() {
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut tail = tail.lock().expect("stderr tail lock poisoned");
                if tail.len() >= 20 {
                    tail.pop_front();
                }
                tail.push_back(line.chars().take(500).collect());
            }
        });
    }
    let tail_text = |tail: &Arc<Mutex<VecDeque<String>>>| {
        let tail = tail.lock().expect("stderr tail lock poisoned");
        if tail.is_empty() {
            String::new()
        } else {
            format!("：{}", tail.iter().cloned().collect::<Vec<_>>().join(" | "))
        }
    };

    let (exited_tx, exited_rx) = watch::channel(false);
    let stop = Arc::new(Notify::new());
    let exit_signal = Arc::clone(&stop);
    // 进程所有权交给监视任务：自行退出或收到停止请求时回收整棵进程树。
    tokio::spawn(async move {
        tokio::select! {
            _ = child.wait() => {}
            _ = exit_signal.notified() => {}
        }
        let _ = child.terminate().await;
        exited_tx.send_replace(true);
    });
    let abort = |stop: &Arc<Notify>| stop.notify_one();

    // 进程在连接前就退出（被杀、入口崩溃）时立即失败，不必等满连接期限。
    let mut exited_early = exited_rx.clone();
    let child_gone = async {
        while !*exited_early.borrow_and_update() {
            if exited_early.changed().await.is_err() {
                break;
            }
        }
    };
    let stream = tokio::select! {
        accepted = tokio::time::timeout(config.connect_timeout, listener.accept()) => match accepted {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                abort(&stop);
                return Err(LaunchError::Endpoint(error));
            }
            Err(_) => {
                abort(&stop);
                return Err(LaunchError::ConnectTimeout(tail_text(&stderr_tail)));
            }
        },
        _ = child_gone => {
            abort(&stop);
            return Err(LaunchError::ExitedBeforeConnect(tail_text(&stderr_tail)));
        }
    };
    drop(listener);

    let mut stream = stream;
    if let Err(message) = handshake(
        &mut stream,
        &config,
        &token,
        generation,
        &approved,
        config.handshake_timeout,
    )
    .await
    {
        abort(&stop);
        return Err(LaunchError::Handshake(message));
    }

    let bound: Arc<Mutex<HashSet<String>>> = Arc::default();
    let (ready_tx, mut ready_rx) = watch::channel(false);
    let handler = Arc::new(HostHandler {
        plugin_id: config.plugin_id.clone(),
        approved: approved.clone(),
        bound: Arc::clone(&bound),
        ready: ready_tx,
    });
    let peer = Peer::spawn(stream, handler, config.limits);

    let ready = tokio::time::timeout(config.ready_timeout, async {
        tokio::select! {
            _ = async { while !*ready_rx.borrow_and_update() { if ready_rx.changed().await.is_err() { break; } } } => {}
            _ = peer.closed() => {}
        }
    })
    .await;
    if ready.is_err() || peer.is_closed() {
        abort(&stop);
        peer.close();
        return Err(LaunchError::NotReady(tail_text(&stderr_tail)));
    }

    Ok(Arc::new(HostInstance {
        plugin_id: config.plugin_id,
        version: config.version,
        digest: config.digest,
        generation,
        peer,
        approved,
        bound,
        stop,
        exited: exited_rx,
    }))
}

async fn handshake<S>(
    stream: &mut S,
    config: &LaunchConfig,
    token: &str,
    generation: u64,
    approved: &HashMap<String, ApprovedHook>,
    timeout: Duration,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let limit = config.limits.frame_bytes;
    let frame = tokio::time::timeout(timeout, read_frame(stream, limit))
        .await
        .map_err(|_| "握手超时".to_string())?
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "宿主在握手前断开".to_string())?;
    let (id, params) = match Message::parse(frame) {
        Ok(Message::Request { id, method, params }) if method == "host.hello" => (id, params),
        _ => return Err("握手前只允许 host.hello 请求".into()),
    };
    match check_hello(&params, token, config) {
        Err((kind, message)) => {
            let _ = write_frame(
                stream,
                &response_err(&id, &RpcError::new(kind, message.clone())),
                limit,
            )
            .await;
            Err(message)
        }
        Ok(features) => {
            let registrations: Vec<_> = config
                .hooks
                .iter()
                .map(|hook| {
                    let approved = approved[&hook.name];
                    json!({"name": hook.name, "mode": hook.mode, "deadlineMs": approved.deadline.as_millis() as u64})
                })
                .collect();
            let result = json!({
                "protocol": Version { major: PROTOCOL_MAJOR, minor: PROTOCOL_MINOR },
                "instance": {
                    "pluginId": config.plugin_id, "version": config.version,
                    "digest": config.digest, "generation": generation,
                },
                "limits": {
                    "frameBytes": limit, "pending": config.limits.pending,
                },
                "grants": config.grants,
                "registrations": {"hooks": registrations},
                "config": {"revision": 1, "values": config.config},
                "features": features,
            });
            write_frame(stream, &response_ok(&id, result), limit)
                .await
                .map_err(|error| error.to_string())
        }
    }
}

fn check_hello(
    params: &Value,
    token: &str,
    config: &LaunchConfig,
) -> Result<Vec<String>, (ErrorKind, String)> {
    let supplied = params
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !constant_time_eq(supplied, token) {
        return Err((ErrorKind::PermissionDenied, "握手令牌无效".into()));
    }
    let protocol: Version =
        serde_json::from_value(params.get("protocol").cloned().unwrap_or(Value::Null))
            .map_err(|_| (ErrorKind::ProtocolError, "缺少 protocol 版本".to_string()))?;
    if protocol.major != PROTOCOL_MAJOR {
        return Err((
            ErrorKind::VersionMismatch,
            format!("协议主版本 {} 不受支持", protocol.major),
        ));
    }
    let extension_api: Version =
        serde_json::from_value(params.get("extensionApi").cloned().unwrap_or(Value::Null))
            .map_err(|_| {
                (
                    ErrorKind::ProtocolError,
                    "缺少 extensionApi 版本".to_string(),
                )
            })?;
    if extension_api.major != 1 {
        return Err((
            ErrorKind::VersionMismatch,
            format!("扩展 API 主版本 {} 不受支持", extension_api.major),
        ));
    }
    // 只有双方都声明的功能才可使用。
    let offered: HashSet<String> = params
        .get("features")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|f| f.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let supported = ["hook.guard"];
    let features: Vec<String> = supported
        .iter()
        .filter(|f| offered.contains(**f))
        .map(|f| f.to_string())
        .collect();
    if config.hooks.iter().any(|hook| hook.mode == HookMode::Guard)
        && !features.iter().any(|f| f == "hook.guard")
    {
        return Err((
            ErrorKind::VersionMismatch,
            "宿主未声明 hook.guard，无法承载 guard 钩子".into(),
        ));
    }
    Ok(features)
}
