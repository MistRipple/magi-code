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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{Notify, watch},
};

#[derive(Clone)]
pub struct LaunchConfig {
    /// 产品随包的固定版本 Node 可执行文件。
    pub node: PathBuf,
    /// 宿主运行时脚本（`runtime/host.mjs`）。
    pub runtime_entry: PathBuf,
    /// 插件入口（包内 `extension/index.mjs`）。
    pub plugin_entry: PathBuf,
    /// 翻译包的兼容运行时目录（随产品发行，含共享依赖）。仅翻译包设置。
    pub compat_root: Option<PathBuf>,
    /// 翻译包的私有数据目录（上游 DSH_HOME 指向这里）。仅翻译包设置。
    pub compat_home: Option<PathBuf>,
    /// 套接字目录（Unix）；Windows 忽略。
    pub endpoint_dir: PathBuf,
    pub plugin_id: String,
    pub version: String,
    pub digest: String,
    pub grants: Vec<String>,
    /// 清单声明并获批的钩子。
    pub hooks: Vec<HookRegistration>,
    /// 清单声明且已获 `tools` 授权的工具 ID；宿主只能绑定其中的工具。
    pub tools: Vec<String>,
    /// 清单声明的用户命令 ID。
    pub commands: Vec<String>,
    pub config: Value,
    pub config_revision: u64,
    /// Node 扩展反向调用 daemon 的公开服务。未配置时，服务方法明确返回未找到。
    pub service: Option<Arc<dyn crate::HostService>>,
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
            compat_root: None,
            compat_home: None,
            endpoint_dir,
            plugin_id: String::new(),
            version: String::new(),
            digest: String::new(),
            grants: Vec::new(),
            hooks: Vec::new(),
            tools: Vec::new(),
            commands: Vec::new(),
            config: Value::Null,
            config_revision: 0,
            service: None,
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

/// 由 daemon 签发的调用租约；连接与父调用结束即失效，不相信插件自报的 owner。
#[derive(Clone)]
struct InvocationLease {
    context: Value,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    model_calls: Arc<std::sync::atomic::AtomicU32>,
}

struct InvocationGuard {
    id: String,
    leases: Arc<Mutex<HashMap<String, InvocationLease>>>,
}
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        if let Some(lease) = self
            .leases
            .lock()
            .expect("invocation leases")
            .remove(&self.id)
        {
            lease.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

struct ServiceLease {
    plugin_id: String,
    lease: String,
    service: Option<Arc<dyn crate::HostService>>,
}
impl Drop for ServiceLease {
    fn drop(&mut self) {
        if let Some(service) = &self.service {
            service.instance_ended(&self.plugin_id, &self.lease);
        }
    }
}

pub struct HostInstance {
    service_lease: ServiceLease,
    pub plugin_id: String,
    pub version: String,
    /// 插件包摘要；提示贡献的片段身份包含它。
    pub digest: String,
    pub generation: u64,
    pub(crate) peer: Peer,
    approved: HashMap<String, ApprovedHook>,
    bound: Arc<Mutex<HashSet<String>>>,
    bound_tools: Arc<Mutex<HashSet<String>>>,
    bound_commands: Arc<Mutex<HashSet<String>>>,
    bound_routes: Arc<Mutex<HashSet<String>>>,
    next_invocation: AtomicU64,
    invocations: Arc<Mutex<HashMap<String, InvocationLease>>>,
    stopping: AtomicBool,
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

    /// 宿主已通过 `register.tool` 绑定处理器的工具才可被调用。
    pub fn is_tool_bound(&self, tool: &str) -> bool {
        self.bound_tools
            .lock()
            .expect("bound tools lock poisoned")
            .contains(tool)
    }

    pub fn is_command_bound(&self, command: &str) -> bool {
        self.bound_commands
            .lock()
            .expect("bound commands lock poisoned")
            .contains(command)
    }

    /// 解析视图请求路径命中的已绑定路由：先精确匹配，再取最长前缀匹配。
    /// 返回绑定键（`exact:/path` 或 `prefix:/path`），即宿主注册时使用的名字。
    pub fn resolve_route(&self, path: &str) -> Option<String> {
        let bound = self
            .bound_routes
            .lock()
            .expect("bound routes lock poisoned");
        let exact = format!("exact:{path}");
        if bound.contains(&exact) {
            return Some(exact);
        }
        bound
            .iter()
            .filter_map(|key| key.strip_prefix("prefix:").map(|prefix| (key, prefix)))
            .filter(|(_, prefix)| path.starts_with(prefix))
            .max_by_key(|(_, prefix)| prefix.len())
            .map(|(key, _)| key.clone())
    }

    /// 向宿主发起一次视图后端调用；取消与期限语义同 [`HostInstance::invoke_tool`]。
    /// `path` 是视图请求的实际路径，宿主按绑定键分发，`input` 里带回实际路径。
    pub async fn invoke_route(
        &self,
        path: &str,
        input: Value,
        ctx: Value,
        timeout: Duration,
        cancelled: impl std::future::Future<Output = ()>,
    ) -> Result<Value, RpcError> {
        let Some(route) = self.resolve_route(path) else {
            return Err(RpcError::new(
                ErrorKind::MethodNotFound,
                format!("后端路由 {path} 未被宿主绑定"),
            ));
        };
        self.invoke_bound(
            "route.invoke",
            "route",
            &route,
            input,
            ctx,
            timeout,
            cancelled,
        )
        .await
    }

    /// 向宿主发起一次工具调用。超时时通知宿主取消并返回 `DeadlineExceeded`。
    ///
    /// `cancelled` 完成表示调用方已取消（用户停止 Turn/任务）：宿主收到 `host.cancel`，
    /// 本调用立即返回 `Cancelled`，不等待宿主响应。
    pub async fn invoke_tool(
        &self,
        tool: &str,
        input: Value,
        ctx: Value,
        timeout: Duration,
        cancelled: impl std::future::Future<Output = ()>,
    ) -> Result<Value, RpcError> {
        if !self.is_tool_bound(tool) {
            return Err(RpcError::new(
                ErrorKind::MethodNotFound,
                format!("工具 {tool} 未被宿主绑定"),
            ));
        }
        self.invoke_bound("tool.invoke", "tool", tool, input, ctx, timeout, cancelled)
            .await
    }

    /// 向宿主发起一次用户命令调用；取消与期限语义同 [`HostInstance::invoke_tool`]。
    pub async fn invoke_command(
        &self,
        command: &str,
        input: Value,
        ctx: Value,
        timeout: Duration,
        cancelled: impl std::future::Future<Output = ()>,
    ) -> Result<Value, RpcError> {
        if !self.is_command_bound(command) {
            return Err(RpcError::new(
                ErrorKind::MethodNotFound,
                format!("命令 {command} 未被宿主绑定"),
            ));
        }
        self.invoke_bound(
            if ctx.get("job").is_some_and(|v| !v.is_null()) {
                "job.run"
            } else {
                "command.invoke"
            },
            "command",
            command,
            input,
            ctx,
            timeout,
            cancelled,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke_bound(
        &self,
        method: &str,
        key: &str,
        name: &str,
        input: Value,
        ctx: Value,
        timeout: Duration,
        cancelled: impl std::future::Future<Output = ()>,
    ) -> Result<Value, RpcError> {
        let base_invocation_id = ctx
            .get("invocationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let invocation_id = format!(
            "{}:{}:{}",
            self.plugin_id,
            self.generation,
            self.next_invocation.fetch_add(1, Ordering::Relaxed)
        );
        let mut ctx = ctx;
        ctx["invocationId"] = json!(invocation_id);
        ctx["callerInvocationId"] = json!(base_invocation_id);
        ctx["generation"] = json!(self.generation);
        ctx["deadlineMs"] = json!(timeout.as_millis() as u64);
        let lease = InvocationLease {
            context: ctx.clone(),
            cancelled: Arc::default(),
            model_calls: Arc::default(),
        };
        // 保留一份与 invocation 表相同的取消句柄。外层 Turn/Task 取消或 daemon
        // deadline 到达时，必须先标记这份句柄，再通知 Node；否则 Node 在收到
        // host.cancel 前发出的反向服务请求仍可能被 daemon 接纳。
        let lease_cancelled = Arc::clone(&lease.cancelled);
        {
            let mut invocations = self.invocations.lock().expect("invocation leases");
            if self.stopping.load(Ordering::Acquire) {
                return Err(RpcError::new(ErrorKind::HostGone, "扩展宿主正在停止"));
            }
            invocations.insert(invocation_id.clone(), lease);
        }
        let _lease = InvocationGuard {
            id: invocation_id.clone(),
            leases: Arc::clone(&self.invocations),
        };
        let request = self.peer.request(
            method,
            json!({key: name, "ctx": ctx, "input": input}),
            timeout,
        );
        let result = tokio::select! {
            result = request => result,
            _ = cancelled => {
                lease_cancelled.store(true, Ordering::Release);
                let _ = self.peer.notify(
                    "host.cancel",
                    json!({"invocationId": invocation_id, "reason": "cancelled"}),
                );
                return Err(RpcError::new(ErrorKind::Cancelled, "调用已取消"));
            }
        };
        // 停用先撤销在途租约。扩展即使在收到 abort 后返回正常文本，也不能把取消改成成功。
        if lease_cancelled.load(Ordering::Acquire) {
            return Err(RpcError::new(
                ErrorKind::Cancelled,
                "扩展宿主停止，调用已取消",
            ));
        }
        if matches!(&result, Err(e) if e.kind() == ErrorKind::DeadlineExceeded) {
            lease_cancelled.store(true, Ordering::Release);
            let _ = self.peer.notify(
                "host.cancel",
                json!({"invocationId": invocation_id, "reason": "deadline"}),
            );
        }
        result
    }

    pub fn is_alive(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !*self.exited.borrow() && !self.peer.is_closed()
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
        self.stopping.store(true, Ordering::Release);
        for lease in self.invocations.lock().expect("invocation leases").values() {
            lease.cancelled.store(true, Ordering::Release);
        }
        if let Some(service) = &self.service_lease.service {
            service.instance_ended(&self.plugin_id, &self.service_lease.lease);
        }
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

    /// daemon 独立计时的健康探测；同步死循环无法响应时由监管回收整棵进程树。
    pub async fn ping(&self) -> Result<(), RpcError> {
        self.peer
            .request("host.ping", json!({}), Duration::from_secs(2))
            .await
            .map(|_| ())
    }

    pub async fn validate_settings(&self, value: Value) -> Result<(), RpcError> {
        self.peer
            .request(
                "settings.validate",
                json!({"generation": self.generation, "value": value}),
                Duration::from_secs(5),
            )
            .await
            .map(|_| ())
    }

    pub fn update_config(&self, revision: u64, values: Value) -> Result<(), RpcError> {
        self.peer.notify(
            "config.changed",
            json!({"revision": revision, "values": values}),
        )
    }
}

struct HostHandler {
    plugin_id: String,
    lease: String,
    approved: HashMap<String, ApprovedHook>,
    bound: Arc<Mutex<HashSet<String>>>,
    approved_tools: HashSet<String>,
    bound_tools: Arc<Mutex<HashSet<String>>>,
    approved_commands: HashSet<String>,
    bound_commands: Arc<Mutex<HashSet<String>>>,
    bound_routes: Arc<Mutex<HashSet<String>>>,
    ready: watch::Sender<bool>,
    service: Option<Arc<dyn crate::HostService>>,
    invocations: Arc<Mutex<HashMap<String, InvocationLease>>>,
}

impl Incoming for HostHandler {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        // 动态绑定与静态绑定共用入口，元数据只从 daemon 的当前实例目录读取。
        if matches!(method.as_str(), "register.tool" | "register.command") {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let is_tool = method == "register.tool";
            let declared = if is_tool {
                self.approved_tools.contains(&name)
            } else {
                self.approved_commands.contains(&name)
            };
            if !declared {
                let service = self.service.clone();
                let plugin = self.plugin_id.clone();
                let lease = self.lease.clone();
                let bound = if is_tool {
                    self.bound_tools.clone()
                } else {
                    self.bound_commands.clone()
                };
                return Box::pin(async move {
                    let service = service.ok_or_else(|| {
                        RpcError::new(ErrorKind::RegistrationDenied, "未声明的贡献")
                    })?;
                    service.request_with_identity(plugin, "registration.check".into(), json!({
                        "_instance": lease, "kind": if is_tool { "tool" } else { "command" }, "name": name,
                    })).await?;
                    bound.lock().expect("bound lock").insert(name);
                    Ok(json!({}))
                });
            }
        }
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
            "register.tool" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.approved_tools.contains(name) {
                    self.bound_tools
                        .lock()
                        .expect("bound tools lock poisoned")
                        .insert(name.to_string());
                    Ok(json!({}))
                } else {
                    Err(RpcError::new(
                        ErrorKind::RegistrationDenied,
                        format!("工具 {name} 未在清单中声明或未获授权"),
                    ))
                }
            }
            // 处理器释放：工具与命令可在运行期按配置上下架（例如作者把某个工具放在开关后面）。
            // 释放只缩小已绑定集合，不能扩大获批集合；未绑定的名字视为幂等成功。
            "unregister.tool" | "unregister.command" | "unregister.route" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let bound = match method.as_str() {
                    "unregister.tool" => &self.bound_tools,
                    "unregister.route" => &self.bound_routes,
                    _ => &self.bound_commands,
                };
                bound.lock().expect("bound lock poisoned").remove(name);
                Ok(json!({}))
            }
            // 视图后端路由：路径由扩展在运行期自报（不在清单里声明），只服务本插件自己的视图。
            // 数量与路径形态受限，避免一个宿主无界占用绑定表。
            "register.route" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !valid_route_key(name) {
                    Err(RpcError::new(
                        ErrorKind::InvalidParams,
                        "后端路由名必须形如 exact:/路径 或 prefix:/路径（不含查询，最长 256 字节）",
                    ))
                } else {
                    let mut bound = self
                        .bound_routes
                        .lock()
                        .expect("bound routes lock poisoned");
                    if !bound.contains(name) && bound.len() >= MAX_BOUND_ROUTES {
                        Err(RpcError::new(
                            ErrorKind::RegistrationDenied,
                            format!("后端路由数量超过上限 {MAX_BOUND_ROUTES}"),
                        ))
                    } else {
                        bound.insert(name.to_string());
                        Ok(json!({}))
                    }
                }
            }
            "register.command" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.approved_commands.contains(name) {
                    self.bound_commands
                        .lock()
                        .expect("bound commands lock poisoned")
                        .insert(name.to_string());
                    Ok(json!({}))
                } else {
                    Err(RpcError::new(
                        ErrorKind::RegistrationDenied,
                        format!("命令 {name} 未在清单中声明"),
                    ))
                }
            }
            _ => {
                let Some(service) = &self.service else {
                    return Box::pin(async move {
                        Err(RpcError::new(ErrorKind::MethodNotFound, method))
                    });
                };
                let service = Arc::clone(service);
                let plugin_id = self.plugin_id.clone();
                let mut params = params;
                let Some(object) = params.as_object_mut() else {
                    return Box::pin(async {
                        Err(RpcError::new(
                            ErrorKind::InvalidParams,
                            "服务参数必须是对象",
                        ))
                    });
                };
                // 下划线字段仅由宿主连接注入，扩展不能伪造。
                if object.keys().any(|key| key.starts_with('_')) {
                    return Box::pin(async {
                        Err(RpcError::new(ErrorKind::InvalidParams, "不允许内部字段"))
                    });
                }
                object.insert("_instance".into(), json!(self.lease));
                if let Some(background) = object.remove("background") {
                    if background != true
                        || !matches!(
                            method.as_str(),
                            "plugins.preview"
                                | "plugins.install"
                                | "plugins.authorize"
                                | "plugins.setEnabled"
                                | "plugins.remove"
                                | "plugins.setVersionExemption"
                        )
                        || object.contains_key("invocationId")
                    {
                        return Box::pin(async {
                            Err(RpcError::new(
                                ErrorKind::InvalidParams,
                                "后台管理标记只适用于独立插件管理调用",
                            ))
                        });
                    }
                    object.insert("_background".into(), json!(true));
                    return service.request_with_identity(plugin_id, method, params);
                }
                let invocation = object.remove("invocationId");
                let lease = invocation.as_ref().and_then(Value::as_str).and_then(|id| {
                    self.invocations
                        .lock()
                        .expect("invocation leases")
                        .get(id)
                        .cloned()
                });
                if invocation.is_some() && lease.is_none() {
                    return Box::pin(async {
                        Err(RpcError::new(ErrorKind::Cancelled, "父调用租约已失效"))
                    });
                }
                if let Some(lease) = lease {
                    if method == "model.invoke"
                        && let Some(budget) = lease.context.pointer("/job/budget")
                    {
                        let max_calls = budget
                            .get("maxModelCalls")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32;
                        let max_output = budget
                            .get("maxOutputTokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let requested = object
                            .get("maxOutputTokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(max_output);
                        if requested == 0
                            || requested > max_output
                            || lease
                                .model_calls
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |calls| {
                                    (calls < max_calls).then_some(calls + 1)
                                })
                                .is_err()
                        {
                            return Box::pin(async {
                                Err(RpcError::new(
                                    ErrorKind::PermissionDenied,
                                    "作业模型预算已耗尽或请求超过授权上限",
                                ))
                            });
                        }
                        object.insert("maxOutputTokens".into(), json!(requested));
                    }
                    object.insert("_context".into(), lease.context);
                    return service.request_scoped(plugin_id, method, params, lease.cancelled);
                }
                if matches!(
                    method.as_str(),
                    "model.invoke"
                        | "task.spawn"
                        | "ask.user"
                        | "notify.user"
                        | "tool.call"
                        | "plugins.install"
                        | "plugins.setEnabled"
                        | "plugins.remove"
                        | "plugins.authorize"
                        | "plugins.setVersionExemption"
                        | "session.snapshot"
                        | "credential.ref"
                        | "blob.put"
                        | "blob.get"
                ) {
                    return Box::pin(async {
                        Err(RpcError::new(
                            ErrorKind::ReentryDenied,
                            "服务需要仍在运行的工具或命令调用；钩子内不允许启动业务动作",
                        ))
                    });
                }
                return service.request_with_identity(plugin_id, method, params);
            }
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

/// 同一宿主可绑定的后端路由上限。
const MAX_BOUND_ROUTES: usize = 128;

fn valid_route_key(key: &str) -> bool {
    let Some(path) = key
        .strip_prefix("exact:")
        .or_else(|| key.strip_prefix("prefix:"))
    else {
        return false;
    };
    path.starts_with('/')
        && path.len() <= 256
        && !path.contains(['?', '#'])
        && path.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
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
    mut config: LaunchConfig,
    generation: u64,
) -> Result<Arc<HostInstance>, LaunchError> {
    if let Some(service) = &config.service
        && let Some(snapshot) = service
            .configuration(config.plugin_id.clone())
            .await
            .map_err(|error| LaunchError::Handshake(error.to_string()))?
    {
        config.config = snapshot["value"].clone();
        config.config_revision = snapshot["version"]
            .as_u64()
            .ok_or_else(|| LaunchError::Handshake("设置快照缺少版本".into()))?;
    }
    let service_lease = ServiceLease {
        plugin_id: config.plugin_id.clone(),
        lease: random_hex(16).map_err(LaunchError::Endpoint)?,
        service: config.service.clone(),
    };
    if let Some(service) = &config.service {
        service
            .instance_started(&config.plugin_id, &service_lease.lease)
            .map_err(|error| LaunchError::Handshake(error.to_string()))?;
    }
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
    if let Some(root) = &config.compat_root {
        command.env("MAGI_EXT_COMPAT_ROOT", root);
    }
    if let Some(home) = &config.compat_home {
        command.env("MAGI_EXT_COMPAT_HOME", home);
    }
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
    // `launch` 被调用方取消时也必须回收已启动的进程，不能只依赖成功返回后的 supervisor。
    struct LaunchGuard(Option<Arc<Notify>>);
    impl Drop for LaunchGuard {
        fn drop(&mut self) {
            if let Some(stop) = &self.0 {
                stop.notify_one();
            }
        }
    }
    let mut launch_guard = LaunchGuard(Some(Arc::clone(&stop)));
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
    let bound_tools: Arc<Mutex<HashSet<String>>> = Arc::default();
    let bound_commands: Arc<Mutex<HashSet<String>>> = Arc::default();
    let bound_routes: Arc<Mutex<HashSet<String>>> = Arc::default();
    let (ready_tx, mut ready_rx) = watch::channel(false);
    let invocations = Arc::default();
    let handler = Arc::new(HostHandler {
        plugin_id: config.plugin_id.clone(),
        lease: service_lease.lease.clone(),
        approved: approved.clone(),
        bound: Arc::clone(&bound),
        approved_tools: config.tools.iter().cloned().collect(),
        bound_tools: Arc::clone(&bound_tools),
        approved_commands: config.commands.iter().cloned().collect(),
        bound_commands: Arc::clone(&bound_commands),
        bound_routes: Arc::clone(&bound_routes),
        ready: ready_tx,
        service: config.service.clone(),
        invocations: Arc::clone(&invocations),
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

    if let Some(service) = &config.service {
        if let Err(error) = service.instance_ready(&config.plugin_id, &service_lease.lease) {
            abort(&stop);
            peer.close();
            return Err(LaunchError::NotReady(error.to_string()));
        }
    }
    launch_guard.0 = None;
    Ok(Arc::new(HostInstance {
        service_lease,
        plugin_id: config.plugin_id,
        version: config.version,
        digest: config.digest,
        generation,
        peer,
        approved,
        bound,
        bound_tools,
        bound_commands,
        bound_routes,
        next_invocation: AtomicU64::new(1),
        invocations,
        stopping: AtomicBool::new(false),
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
                "registrations": {"hooks": registrations, "tools": config.tools, "commands": config.commands},
                "config": {"revision": config.config_revision, "values": config.config},
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
    let supported = [
        "hook.guard",
        "plugin.management",
        "plugin.management.background",
        "session.directory",
        "images.store",
    ];
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
