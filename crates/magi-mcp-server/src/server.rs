//! MCP 请求分发与调用管线。
//!
//! 服务是**薄的协议与策略层**：不复制工具实现，也不持有审批与账本。一次 `tools/call`
//! 的顺序固定为：
//!
//! MCP 客户端使用完整工具目录（GPT Web 槽位按设置过滤）→ 归属解析 → 工作区路径校验 → 审批处置 → 后端执行 → 审计。
//!
//! 任何一步失败都以 MCP 工具错误返回，后续步骤不执行；网络与本机客户端走同一条管线。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use serde_json::{Value, json};

use crate::catalog::{
    DynamicTool, ResolvedTool, ToolDescriptor, ToolSchemaProvider, build_catalog,
    mapping_for_public_name, tools_list_result,
};
use crate::path_guard::{PathGuard, PathRequest};
use crate::profile::ApprovalMode;
use crate::profile::{Disposition, Profile, ToolClass};
use crate::protocol::{
    JSONRPC_INVALID_PARAMS, JSONRPC_INVALID_REQUEST, JSONRPC_METHOD_NOT_FOUND, JsonRpcRequest,
    error, initialize_result, ping_result, success, tool_failure, tool_success,
};
use crate::token::{AttributionMode, TokenRecord};

/// 单次调用参数的上限（字节）。
pub const MAX_ARGUMENT_BYTES: usize = 4 * 1024 * 1024;
/// 单次调用结果的上限（字节）。超出时带提示截断，不做静默截断。
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 已认证的调用方。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientPrincipal {
    pub token_id: String,
    pub token_prefix: String,
    pub client_name: String,
    pub workspace_id: String,
    pub profile: Profile,
    pub approval_mode: ApprovalMode,
    pub attribution: AttributionMode,
}

impl From<&TokenRecord> for ClientPrincipal {
    fn from(record: &TokenRecord) -> Self {
        Self {
            token_id: record.token_id.clone(),
            token_prefix: record.prefix.clone(),
            client_name: record.client_name.clone(),
            workspace_id: record.workspace_id.clone(),
            // 令牌统一开放完整工具目录；工具档只属于 GPT Web 槽位的 principal。
            profile: Profile::Exec,
            approval_mode: record.approval_mode,
            attribution: record.attribution,
        }
    }
}

/// 调用归属到哪里。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttributionTarget {
    /// 该令牌自己的外部工具会话。
    ExternalSession { token_id: String },
    /// GPT Web 槽位拥有者进行中的 turn。
    WebSlotTurn { session_id: String, turn_id: String },
}

/// 归属被拒绝（例如 `follow_web_slot` 时没有进行中的 turn）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributionRefusal(pub String);

pub trait AttributionResolver: Send + Sync {
    fn resolve(&self, principal: &ClientPrincipal)
    -> Result<AttributionTarget, AttributionRefusal>;
}

/// 默认实现：`external` 归到外部工具会话；`follow_web_slot` 没有 GPT Web 联动时一律拒绝。
#[derive(Debug, Default)]
pub struct ExternalOnlyAttribution;

impl AttributionResolver for ExternalOnlyAttribution {
    fn resolve(
        &self,
        principal: &ClientPrincipal,
    ) -> Result<AttributionTarget, AttributionRefusal> {
        match principal.attribution {
            AttributionMode::External => Ok(AttributionTarget::ExternalSession {
                token_id: principal.token_id.clone(),
            }),
            AttributionMode::FollowWebSlot => Err(AttributionRefusal(
                "当前没有可归属的进行中的 GPT Web 对话".to_string(),
            )),
        }
    }
}

/// 工作区根目录解析（令牌只绑定工作区 id）。
pub trait WorkspaceResolver: Send + Sync {
    fn root_of(&self, workspace_id: &str) -> Option<PathBuf>;

    fn name_of(&self, _workspace_id: &str) -> Option<String> {
        None
    }
}

/// 交给后端执行的一次调用。
#[derive(Clone, Debug)]
pub struct ToolInvocation {
    pub principal: ClientPrincipal,
    pub public_name: String,
    pub internal_name: String,
    pub class: ToolClass,
    pub arguments: Value,
    /// 该调用是否需要 Magi 界面里的人工确认（由后端负责发起与等待）。
    pub requires_approval: bool,
    pub attribution: AttributionTarget,
    /// 已通过校验的规范化路径。
    pub resolved_paths: Vec<PathBuf>,
    /// 请求级取消信号；并发租约由服务端保留到宿主结算完成。
    pub cancellation: RequestCancellation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationOutcome {
    pub text: String,
    pub is_error: bool,
}

/// 后端：真正的工具执行与审批，由宿主实现（工具运行时、审批注册表、变更账本）。
pub trait ToolBackend: Send + Sync {
    fn schemas(&self) -> &dyn ToolSchemaProvider;

    /// 该工具的参数里有哪些路径访问；由宿主用现有的路径声明能力给出（相对路径按工作区根解析）。
    /// 宿主提供的额外工具（项目允许的其余内置工具、下游 MCP 工具、Skill handler）。
    /// 每次 `tools/list` 与 `tools/call` 都会重新取，使宿主侧的启停、连接状态立即体现。
    fn dynamic_tools(&self) -> Vec<DynamicTool> {
        Vec::new()
    }

    fn path_requests(
        &self,
        internal_name: &str,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Vec<PathRequest>;

    fn invoke<'a>(&'a self, invocation: ToolInvocation) -> BoxFuture<'a, InvocationOutcome>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    Executed { is_error: bool },
    Denied { reason: String },
}

/// 审计事件：不含令牌原文，不含文件正文，只含路径 / 工具的摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    pub token_id: String,
    pub client_name: String,
    pub workspace_id: String,
    pub tool: String,
    pub requires_approval: bool,
    pub paths: Vec<String>,
    pub outcome: AuditOutcome,
    pub at_ms: u64,
}

pub trait AuditSink: Send + Sync {
    fn record(&self, event: AuditEvent);
}

#[derive(Debug, Default)]
pub struct NoopAudit;

impl AuditSink for NoopAudit {
    fn record(&self, _event: AuditEvent) {}
}

/// 同一个客户端同时进行的 `tools/call` 上限（含正在等待人工审批的调用）。回环请求同样生效：
/// 本机一个失控的客户端不应该能无限并发地占用工具运行时和审批队列。
pub const MAX_CONCURRENT_CALLS_PER_CLIENT: usize = 8;

/// 请求 ID 与取消信号复用并发登记；令牌下所有连接共用并发上限。
#[derive(Default, Debug)]
struct InFlightCalls(
    std::sync::Mutex<std::collections::HashMap<(u64, String, String), Weak<AtomicBool>>>,
);

#[derive(Clone, Debug)]
pub struct RequestCancellation(Arc<AtomicBool>);

impl RequestCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
    fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub(crate) struct InFlightGuard {
    cancellation: RequestCancellation,
    key: (u64, String, String),
    calls: Arc<InFlightCalls>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.calls
            .0
            .lock()
            .expect("in-flight counter poisoned")
            .remove(&self.key);
    }
}

struct CancelOnDrop(RequestCancellation);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// 每条 stdio 连接或 HTTP 传输会话拥有独立请求身份；释放时只取消该 scope 的调用。
pub(crate) struct ConnectionScope {
    pub(crate) id: u64,
    calls: Arc<InFlightCalls>,
}

impl ConnectionScope {
    pub(crate) fn cancel(&self) {
        let calls: Vec<_> = self
            .calls
            .0
            .lock()
            .expect("in-flight counter poisoned")
            .iter()
            .filter(|((scope, _, _), _)| *scope == self.id)
            .map(|(_, call)| call.clone())
            .collect();
        for call in calls {
            if let Some(call) = call.upgrade() {
                call.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl Drop for ConnectionScope {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl InFlightCalls {
    fn acquire(
        self: &Arc<Self>,
        scope: u64,
        client: &str,
        id: &Value,
    ) -> Result<InFlightGuard, &'static str> {
        let mut counts = self.0.lock().expect("in-flight counter poisoned");
        let key = (scope, client.to_string(), id.to_string());
        if counts.contains_key(&key) {
            return Err("该请求 ID 已有进行中的调用");
        }
        if counts
            .keys()
            .filter(|(_, token, _)| token == client)
            .count()
            >= MAX_CONCURRENT_CALLS_PER_CLIENT
        {
            return Err("该客户端同时进行的调用已达上限，请等当前调用完成后再试");
        }
        let flag = Arc::new(AtomicBool::new(false));
        counts.insert(key.clone(), Arc::downgrade(&flag));
        Ok(InFlightGuard {
            cancellation: RequestCancellation(flag),
            key,
            calls: self.clone(),
        })
    }

    fn cancel(&self, scope: u64, client: &str, id: &Value) {
        let call = self
            .0
            .lock()
            .expect("in-flight counter poisoned")
            .get(&(scope, client.to_string(), id.to_string()))
            .cloned();
        if let Some(call) = call.and_then(|call| call.upgrade()) {
            call.store(true, Ordering::SeqCst);
        }
    }
}

pub struct McpServer {
    name: String,
    version: String,
    backend: Arc<dyn ToolBackend>,
    workspaces: Arc<dyn WorkspaceResolver>,
    attribution: Arc<dyn AttributionResolver>,
    audit: Arc<dyn AuditSink>,
    in_flight: Arc<InFlightCalls>,
}

impl McpServer {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        backend: Arc<dyn ToolBackend>,
        workspaces: Arc<dyn WorkspaceResolver>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            backend,
            workspaces,
            attribution: Arc::new(ExternalOnlyAttribution),
            audit: Arc::new(NoopAudit),
            in_flight: Arc::new(InFlightCalls::default()),
        }
    }

    pub fn with_attribution(mut self, attribution: Arc<dyn AttributionResolver>) -> Self {
        self.attribution = attribution;
        self
    }

    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = audit;
        self
    }

    /// 当前令牌可见的工具目录。
    pub fn catalog_for(&self, principal: &ClientPrincipal) -> Vec<ToolDescriptor> {
        build_catalog(
            principal.profile,
            self.backend.schemas(),
            &self.backend.dynamic_tools(),
        )
    }

    /// 处理一条已通过认证的 JSON-RPC 请求。通知返回 `None`。
    pub async fn handle(
        &self,
        principal: &ClientPrincipal,
        request: JsonRpcRequest,
        now_ms: u64,
    ) -> Option<Value> {
        let registration = self.prepare_request(principal, &request, 0);
        self.handle_prepared(principal, request, now_ms, registration)
            .await
    }

    pub(crate) fn connection_scope(&self) -> ConnectionScope {
        static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
        ConnectionScope {
            id: NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
            calls: self.in_flight.clone(),
        }
    }

    /// stdio 在派发异步执行之前登记，保证紧随其后的取消通知不会越过登记。
    pub(crate) fn prepare_request(
        &self,
        principal: &ClientPrincipal,
        request: &JsonRpcRequest,
        scope: u64,
    ) -> Option<Result<InFlightGuard, &'static str>> {
        if !request.is_well_formed() {
            return None;
        }
        if request.is_notification()
            && request.method == "notifications/cancelled"
            && let Some(id) = request
                .params
                .as_ref()
                .and_then(|params| params.get("requestId"))
            && (id.is_string() || id.is_number())
        {
            self.in_flight.cancel(scope, &principal.token_id, id);
        }
        (request.method == "tools/call" && !request.is_notification()).then(|| {
            self.in_flight
                .acquire(scope, &principal.token_id, &request.response_id())
        })
    }

    pub(crate) async fn handle_prepared(
        &self,
        principal: &ClientPrincipal,
        request: JsonRpcRequest,
        now_ms: u64,
        registration: Option<Result<InFlightGuard, &'static str>>,
    ) -> Option<Value> {
        if !request.is_well_formed() {
            return Some(error(
                request.response_id(),
                JSONRPC_INVALID_REQUEST,
                "无效的 JSON-RPC 请求",
            ));
        }
        if request.is_notification() {
            return None;
        }
        let id = request.response_id();
        match request.method.as_str() {
            "initialize" => {
                let instructions = if principal.attribution == AttributionMode::External {
                    let Some(root) = self.workspaces.root_of(&principal.workspace_id) else {
                        return Some(error(id, JSONRPC_INVALID_PARAMS, "绑定的工作区不可用"));
                    };
                    let workspace = json!({
                        "name": self.workspaces.name_of(&principal.workspace_id)
                            .unwrap_or_else(|| principal.workspace_id.clone()),
                        "root": root,
                    });
                    Some(format!(
                        "This Magi MCP connection is bound to workspace {workspace}. \
                         Paths refer to the machine running Magi, not the client machine. \
                         Resolve relative paths against this workspace root. \
                         The target does not change when the client opens another project. \
                         Use a separate Magi MCP connection for another workspace."
                    ))
                } else {
                    None
                };
                Some(success(
                    id,
                    initialize_result(&self.name, &self.version, instructions.as_deref()),
                ))
            }
            "ping" => Some(success(id, ping_result())),
            "tools/list" => Some(success(id, tools_list_result(&self.catalog_for(principal)))),
            "tools/call" => {
                let params = request.params.unwrap_or(Value::Null);
                match self
                    .call_tool(
                        principal,
                        &params,
                        now_ms,
                        registration.expect("tool request registered"),
                    )
                    .await
                {
                    Ok(result) => Some(success(id, result)),
                    Err(message) => Some(error(id, JSONRPC_INVALID_PARAMS, message)),
                }
            }
            other => Some(error(
                id,
                JSONRPC_METHOD_NOT_FOUND,
                format!("不支持的方法: {other}"),
            )),
        }
    }

    /// 参数结构错误返回 `Err`（JSON-RPC 错误）；其余一切失败都是工具错误结果。
    async fn call_tool(
        &self,
        principal: &ClientPrincipal,
        params: &Value,
        now_ms: u64,
        registration: Result<InFlightGuard, &'static str>,
    ) -> Result<Value, String> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err("缺少工具名 name".to_string());
        };
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(value @ Value::Object(_)) => value.clone(),
            Some(_) => return Err("arguments 必须是对象".to_string()),
        };

        // 目录里没有的工具（含该权限档不可见的）统一按“不存在”处理，不泄露其存在。
        let resolved = mapping_for_public_name(name)
            .map(ResolvedTool::from)
            .or_else(|| {
                self.backend
                    .dynamic_tools()
                    .iter()
                    .find(|tool| tool.public_name == name)
                    .map(ResolvedTool::from)
            })
            .filter(|tool| principal.profile.allows(tool.class));
        let Some(mapping) = resolved else {
            return Ok(tool_failure(format!("未知工具: {name}")));
        };

        let paths_summary = |paths: &[PathBuf]| -> Vec<String> {
            paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        };
        let deny = |reason: String, paths: Vec<String>, requires_approval: bool| -> Value {
            self.audit.record(AuditEvent {
                token_id: principal.token_id.clone(),
                client_name: principal.client_name.clone(),
                workspace_id: principal.workspace_id.clone(),
                tool: mapping.public_name.clone(),
                requires_approval,
                paths,
                outcome: AuditOutcome::Denied {
                    reason: reason.clone(),
                },
                at_ms: now_ms,
            });
            tool_failure(reason)
        };

        if serde_json::to_vec(&arguments).map_or(0, |bytes| bytes.len()) > MAX_ARGUMENT_BYTES {
            return Ok(deny(
                format!("参数超过 {MAX_ARGUMENT_BYTES} 字节上限"),
                Vec::new(),
                false,
            ));
        }

        // 并发上限：占住名额直到本次调用结束（含等待审批的时间）。
        let in_flight = match registration {
            Ok(guard) => guard,
            Err(message) => return Ok(deny(message.to_string(), Vec::new(), false)),
        };
        let cancellation = in_flight.cancellation.clone();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());

        let attribution = match self.attribution.resolve(principal) {
            Ok(target) => target,
            Err(AttributionRefusal(reason)) => return Ok(deny(reason, Vec::new(), false)),
        };

        let Some(root) = self.workspaces.root_of(&principal.workspace_id) else {
            return Ok(deny(
                "令牌绑定的工作区不可用".to_string(),
                Vec::new(),
                false,
            ));
        };
        let guard = match PathGuard::new(&root) {
            Ok(guard) => guard,
            Err(violation) => return Ok(deny(violation.to_string(), Vec::new(), false)),
        };
        let requests = self
            .backend
            .path_requests(&mapping.internal_name, &arguments, &root);
        let resolved_paths = match guard.check_all(&requests) {
            Ok(paths) => paths,
            Err(violation) => {
                return Ok(deny(
                    violation.to_string(),
                    requests.iter().map(|request| request.raw.clone()).collect(),
                    false,
                ));
            }
        };

        let requires_approval = match principal.attribution {
            AttributionMode::External => principal.approval_mode == ApprovalMode::Ask,
            AttributionMode::FollowWebSlot => requires_approval(principal.profile, &mapping),
        };
        let invocation = ToolInvocation {
            principal: principal.clone(),
            public_name: mapping.public_name.clone(),
            internal_name: mapping.internal_name.clone(),
            class: mapping.class,
            arguments,
            requires_approval,
            attribution,
            resolved_paths: resolved_paths.clone(),
            cancellation,
        };
        let backend = self.backend.clone();
        let audit = self.audit.clone();
        let event = AuditEvent {
            token_id: principal.token_id.clone(),
            client_name: principal.client_name.clone(),
            workspace_id: principal.workspace_id.clone(),
            tool: mapping.public_name.clone(),
            requires_approval,
            paths: paths_summary(&resolved_paths),
            outcome: AuditOutcome::Executed { is_error: false },
            at_ms: now_ms,
        };
        // 请求被丢弃时通知取消，但后端必须结算实际副作用、时间线和审计。
        let outcome = tokio::spawn(async move {
            let _in_flight = in_flight;
            let outcome = backend.invoke(invocation).await;
            audit.record(AuditEvent {
                outcome: AuditOutcome::Executed {
                    is_error: outcome.is_error,
                },
                ..event
            });
            outcome
        })
        .await
        .unwrap_or_else(|_| InvocationOutcome {
            text: "工具执行意外中断".into(),
            is_error: true,
        });

        let text = truncate_with_notice(outcome.text, MAX_RESULT_BYTES);
        Ok(if outcome.is_error {
            tool_failure(text)
        } else {
            tool_success(text)
        })
    }
}

fn requires_approval(profile: Profile, mapping: &ResolvedTool) -> bool {
    profile.decide(mapping.class) == Disposition::RequireApproval
}

/// 超出上限时在字符边界截断并明确提示，不做静默截断。
fn truncate_with_notice(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n\n[输出已截断：共 {} 字节，只返回前 {} 字节]",
        &text[..cut],
        text.len(),
        cut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ToolSchema;
    use crate::path_guard::PathAccess;
    use std::sync::Mutex;
    use std::time::Duration;

    struct Schemas;
    impl ToolSchemaProvider for Schemas {
        fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
            Some(ToolSchema {
                description: format!("{internal_name} 说明"),
                input_schema: json!({ "type": "object" }),
            })
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        calls: Mutex<Vec<ToolInvocation>>,
        reply: Mutex<Option<InvocationOutcome>>,
    }

    impl ToolBackend for FakeBackend {
        fn schemas(&self) -> &dyn ToolSchemaProvider {
            static SCHEMAS: Schemas = Schemas;
            &SCHEMAS
        }
        fn path_requests(
            &self,
            internal_name: &str,
            arguments: &Value,
            _workspace_root: &Path,
        ) -> Vec<PathRequest> {
            let access = if internal_name == "file_read" {
                PathAccess::Read
            } else {
                PathAccess::Write
            };
            arguments
                .get("path")
                .and_then(Value::as_str)
                .map(|raw| PathRequest {
                    raw: raw.to_string(),
                    access,
                })
                .into_iter()
                .collect()
        }
        fn invoke<'a>(&'a self, invocation: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
            self.calls.lock().unwrap().push(invocation);
            let reply = self
                .reply
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(InvocationOutcome {
                    text: "ok".to_string(),
                    is_error: false,
                });
            Box::pin(async move { reply })
        }
    }

    struct Root(PathBuf);
    impl WorkspaceResolver for Root {
        fn root_of(&self, workspace_id: &str) -> Option<PathBuf> {
            (workspace_id == "ws").then(|| self.0.clone())
        }
    }

    #[derive(Default)]
    struct Audit(Mutex<Vec<AuditEvent>>);
    impl AuditSink for Audit {
        fn record(&self, event: AuditEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    fn principal(profile: Profile) -> ClientPrincipal {
        ClientPrincipal {
            token_id: "tok-1".to_string(),
            token_prefix: "magi_mcp_abcdef".to_string(),
            client_name: "Cursor".to_string(),
            workspace_id: "ws".to_string(),
            profile,
            approval_mode: ApprovalMode::Ask,
            attribution: AttributionMode::External,
        }
    }

    fn request(method: &str, id: Option<Value>, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params: Some(params),
        }
    }

    fn call(name: &str, arguments: Value) -> JsonRpcRequest {
        request(
            "tools/call",
            Some(json!(1)),
            json!({ "name": name, "arguments": arguments }),
        )
    }

    struct Harness {
        _dir: tempfile::TempDir,
        server: McpServer,
        backend: Arc<FakeBackend>,
        audit: Arc<Audit>,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let backend = Arc::new(FakeBackend::default());
        let audit = Arc::new(Audit::default());
        let server = McpServer::new(
            "magi-mcp",
            "test",
            backend.clone(),
            Arc::new(Root(dir.path().to_path_buf())),
        )
        .with_audit(audit.clone());
        Harness {
            _dir: dir,
            server,
            backend,
            audit,
        }
    }

    fn is_error(response: &Value) -> bool {
        response["result"]["isError"] == json!(true)
    }

    #[tokio::test]
    async fn initialize_ping_and_notifications_follow_the_protocol() {
        let h = harness();
        let p = principal(Profile::ReadOnly);
        let init = h
            .server
            .handle(&p, request("initialize", Some(json!(1)), json!({})), 1)
            .await
            .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        let instructions = init["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains(&h._dir.path().display().to_string()));
        assert!(instructions.contains("Resolve relative paths against this workspace root"));
        assert!(instructions.contains("does not change when the client opens another project"));
        assert_eq!(
            init["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        assert!(
            h.server
                .handle(&p, request("notifications/initialized", None, json!({})), 1)
                .await
                .is_none()
        );
        let pong = h
            .server
            .handle(&p, request("ping", Some(json!("x")), json!({})), 1)
            .await
            .unwrap();
        assert_eq!(pong["id"], "x");
        let missing = h
            .server
            .handle(&p, request("resources/list", Some(json!(2)), json!({})), 1)
            .await
            .unwrap();
        assert_eq!(missing["error"]["code"], JSONRPC_METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn initialize_never_falls_back_to_another_workspace_or_binds_a_web_slot() {
        let h = harness();
        let mut p = principal(Profile::ReadOnly);
        p.workspace_id = "unregistered".into();
        let response = h
            .server
            .handle(&p, request("initialize", Some(json!(1)), json!({})), 1)
            .await
            .unwrap();
        assert!(response.get("error").is_some());
        assert!(
            !response
                .to_string()
                .contains(&h._dir.path().display().to_string())
        );

        p.attribution = AttributionMode::FollowWebSlot;
        let response = h
            .server
            .handle(&p, request("initialize", Some(json!(2)), json!({})), 1)
            .await
            .unwrap();
        assert!(response["result"].get("instructions").is_none());
        assert_eq!(response["result"]["serverInfo"]["name"], "magi-mcp");
    }

    #[tokio::test]
    async fn malformed_requests_are_rejected_without_touching_the_backend() {
        let h = harness();
        let mut bad = request("tools/list", Some(json!(true)), json!({}));
        bad.jsonrpc = "2.0".to_string();
        let response = h
            .server
            .handle(&principal(Profile::Exec), bad, 1)
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], JSONRPC_INVALID_REQUEST);
        assert!(h.backend.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tools_list_is_filtered_by_profile() {
        let h = harness();
        let list = |profile| async move {
            let response = h
                .server
                .handle(
                    &principal(profile),
                    request("tools/list", Some(json!(1)), json!({})),
                    1,
                )
                .await
                .unwrap();
            response["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        // harness 只借用一次，这里分别构造。
        let read_only = list(Profile::ReadOnly).await;
        assert!(read_only.contains(&"magi.fs.read".to_string()));
        assert!(!read_only.contains(&"magi.fs.write".to_string()));
    }

    #[tokio::test]
    async fn ask_mode_requires_approval_for_every_external_tool_call() {
        let h = harness();
        let p = principal(Profile::Exec);
        for (index, request) in [
            call("magi.fs.read", json!({"path": "a.txt"})),
            call("magi.fs.write", json!({"path": "b.txt", "content": "x"})),
            call("magi.fs.remove", json!({"path": "a.txt"})),
            call("magi.shell.exec", json!({"command": "printf hi"})),
        ]
        .into_iter()
        .enumerate()
        {
            let response = h.server.handle(&p, request, index as u64).await.unwrap();
            assert!(!is_error(&response));
        }

        let calls = h.backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|call| call.requires_approval));
        assert_eq!(calls[1].internal_name, "file_write");
        assert_eq!(
            calls[1].attribution,
            AttributionTarget::ExternalSession {
                token_id: "tok-1".to_string()
            }
        );
        drop(calls);

        let events = h.audit.0.lock().unwrap();
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|event| event.requires_approval));
    }

    #[tokio::test]
    async fn always_approval_mode_skips_prompts_for_the_full_tool_surface() {
        let h = harness();
        let mut p = principal(Profile::Exec);
        p.approval_mode = ApprovalMode::Always;
        h.server
            .handle(
                &p,
                call("magi.fs.write", json!({"path": "b.txt", "content": "x"})),
                1,
            )
            .await
            .unwrap();
        h.server
            .handle(&p, call("magi.fs.remove", json!({"path": "a.txt"})), 2)
            .await
            .unwrap();
        h.server
            .handle(
                &p,
                call("magi.shell.exec", json!({"command": "printf open"})),
                3,
            )
            .await
            .unwrap();
        h.server
            .handle(&p, call("magi.fs.read", json!({"path": "a.txt"})), 4)
            .await
            .unwrap();
        let calls = h.backend.calls.lock().unwrap();
        assert!(calls.iter().all(|call| !call.requires_approval));
    }

    #[tokio::test]
    async fn tools_outside_the_profile_look_unknown_and_never_reach_the_backend() {
        let h = harness();
        for (profile, name) in [
            (Profile::ReadOnly, "magi.fs.write"),
            (Profile::Edit, "magi.shell.exec"),
            (Profile::Exec, "no.such.tool"),
            (Profile::Exec, "file_write"),
        ] {
            let response = h
                .server
                .handle(&principal(profile), call(name, json!({"path": "a.txt"})), 1)
                .await
                .unwrap();
            assert!(is_error(&response), "{profile:?} {name}");
            assert!(
                response["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("未知工具")
            );
        }
        assert!(h.backend.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn path_violations_are_tool_errors_and_are_audited_as_denied() {
        let h = harness();
        let response = h
            .server
            .handle(
                &principal(Profile::Edit),
                call(
                    "magi.fs.write",
                    json!({"path": "../escape.txt", "content": "x"}),
                ),
                9,
            )
            .await
            .unwrap();
        assert!(is_error(&response));
        assert!(h.backend.calls.lock().unwrap().is_empty());
        let events = h.audit.0.lock().unwrap();
        assert!(matches!(events[0].outcome, AuditOutcome::Denied { .. }));
        assert_eq!(events[0].paths, vec!["../escape.txt".to_string()]);
    }

    #[tokio::test]
    async fn follow_web_slot_without_an_active_turn_is_refused() {
        let h = harness();
        let mut p = principal(Profile::Edit);
        p.attribution = AttributionMode::FollowWebSlot;
        let response = h
            .server
            .handle(&p, call("magi.fs.read", json!({"path": "a.txt"})), 1)
            .await
            .unwrap();
        assert!(is_error(&response));
        assert!(h.backend.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_call_parameters_are_jsonrpc_errors() {
        let h = harness();
        let p = principal(Profile::Edit);
        let no_name = h
            .server
            .handle(&p, request("tools/call", Some(json!(1)), json!({})), 1)
            .await
            .unwrap();
        assert_eq!(no_name["error"]["code"], JSONRPC_INVALID_PARAMS);
        let bad_args = h
            .server
            .handle(
                &p,
                request(
                    "tools/call",
                    Some(json!(2)),
                    json!({"name": "magi.fs.read", "arguments": [1]}),
                ),
                1,
            )
            .await
            .unwrap();
        assert_eq!(bad_args["error"]["code"], JSONRPC_INVALID_PARAMS);
    }

    #[tokio::test]
    async fn backend_errors_and_oversized_results_are_reported_explicitly() {
        let h = harness();
        *h.backend.reply.lock().unwrap() = Some(InvocationOutcome {
            text: "x".repeat(MAX_RESULT_BYTES + 10),
            is_error: false,
        });
        let big = h
            .server
            .handle(
                &principal(Profile::ReadOnly),
                call("magi.fs.read", json!({"path": "a.txt"})),
                1,
            )
            .await
            .unwrap();
        let text = big["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("输出已截断"));

        *h.backend.reply.lock().unwrap() = Some(InvocationOutcome {
            text: "denied by user".to_string(),
            is_error: true,
        });
        let failed = h
            .server
            .handle(
                &principal(Profile::Edit),
                call("magi.fs.write", json!({"path": "b.txt", "content": "x"})),
                2,
            )
            .await
            .unwrap();
        assert!(is_error(&failed));
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        let text = "汉字".repeat(10);
        let cut = truncate_with_notice(text.clone(), 7);
        assert!(cut.starts_with("汉字"));
        assert!(cut.contains("输出已截断"));
        assert_eq!(truncate_with_notice("short".to_string(), 100), "short");
    }

    /// 调用会一直挂起直到被放行的后端，用来占住并发名额。
    #[derive(Default)]
    struct BlockingBackend {
        entered: tokio::sync::Notify,
        gate: tokio::sync::Notify,
    }

    impl ToolBackend for BlockingBackend {
        fn schemas(&self) -> &dyn ToolSchemaProvider {
            static SCHEMAS: Schemas = Schemas;
            &SCHEMAS
        }
        fn path_requests(&self, _: &str, _: &Value, _: &Path) -> Vec<PathRequest> {
            Vec::new()
        }
        fn invoke<'a>(&'a self, _invocation: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
            Box::pin(async move {
                self.entered.notify_one();
                self.gate.notified().await;
                InvocationOutcome {
                    text: "ok".to_string(),
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn dropped_request_keeps_its_permit_until_actual_execution_and_audit_settle() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let audit = Arc::new(Audit::default());
        let server = Arc::new(
            McpServer::new(
                "magi",
                "test",
                backend.clone(),
                Arc::new(Root(dir.path().into())),
            )
            .with_audit(audit.clone()),
        );
        let running = {
            let server = server.clone();
            tokio::spawn(async move {
                server
                    .handle(
                        &principal(Profile::Exec),
                        call("magi.fs.read", json!({})),
                        1,
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
            .await
            .unwrap();
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        {
            let calls = server.in_flight.0.lock().unwrap();
            assert_eq!(calls.len(), 1, "执行尚未结算，不得提前归还并发名额");
            assert!(
                calls
                    .values()
                    .next()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .load(Ordering::SeqCst)
            );
        }
        backend.gate.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !server.in_flight.0.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let events = audit.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(
                events[0].outcome,
                AuditOutcome::Executed { is_error: false }
            ),
            "已完成的真实副作用不能伪报取消"
        );
    }

    #[tokio::test]
    async fn one_client_cannot_exceed_its_concurrent_call_limit_but_others_are_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(BlockingBackend::default());
        let server = Arc::new(McpServer::new(
            "magi-mcp",
            "test",
            backend.clone(),
            Arc::new(Root(dir.path().to_path_buf())),
        ));
        let busy = principal(Profile::ReadOnly);
        let mut pending = Vec::new();
        for id in 0..MAX_CONCURRENT_CALLS_PER_CLIENT {
            let server = Arc::clone(&server);
            let busy = busy.clone();
            pending.push(tokio::spawn(async move {
                server
                    .handle(
                        &busy,
                        request(
                            "tools/call",
                            Some(json!(id)),
                            json!({"name":"magi.fs.read"}),
                        ),
                        1,
                    )
                    .await
            }));
        }
        // 等所有调用都占住名额。
        for _ in 0..200 {
            if server
                .in_flight
                .0
                .lock()
                .unwrap()
                .keys()
                .filter(|(_, token, _)| token == "tok-1")
                .count()
                == MAX_CONCURRENT_CALLS_PER_CLIENT
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let refused = server
            .handle(
                &busy,
                request(
                    "tools/call",
                    Some(json!(99)),
                    json!({"name":"magi.fs.read"}),
                ),
                1,
            )
            .await
            .unwrap();
        assert!(is_error(&refused), "{refused}");
        assert!(refused.to_string().contains("已达上限"), "{refused}");

        // 另一个客户端不受影响（这里只验证没有被计数挡住：它同样会挂起，所以限时等待）。
        let mut other = busy.clone();
        other.token_id = "tok-2".to_string();
        let other_call = tokio::time::timeout(
            Duration::from_millis(100),
            server.handle(&other, call("magi.fs.read", json!({})), 1),
        )
        .await;
        assert!(
            other_call.is_err(),
            "其他客户端的调用应正常进入后端而不是被拒绝"
        );

        // 放行后名额归还，新的调用又能进入。
        backend.gate.notify_waiters();
        for task in pending {
            let response = task.await.unwrap().unwrap();
            assert!(!is_error(&response), "{response}");
        }
        assert!(
            server.in_flight.0.lock().unwrap().is_empty(),
            "名额必须全部归还"
        );
    }

    #[tokio::test]
    async fn cancellation_is_bound_to_connection_client_and_typed_request_id() {
        let h = harness();
        let p = principal(Profile::Exec);
        let first = h.server.connection_scope();
        let second = h.server.connection_scope();
        let numeric = request("tools/call", Some(json!(1)), json!({"name":"magi.fs.read"}));
        let text = request(
            "tools/call",
            Some(json!("1")),
            json!({"name":"magi.fs.read"}),
        );
        let a = h
            .server
            .prepare_request(&p, &numeric, first.id)
            .unwrap()
            .unwrap();
        let b = h
            .server
            .prepare_request(&p, &numeric, second.id)
            .unwrap()
            .unwrap();
        let c = h
            .server
            .prepare_request(&p, &text, first.id)
            .unwrap()
            .unwrap();
        assert!(
            h.server
                .prepare_request(&p, &numeric, first.id)
                .unwrap()
                .is_err()
        );
        let cancellation = request("notifications/cancelled", None, json!({"requestId":1}));
        let mut other = p.clone();
        other.token_id = "other".into();
        h.server.prepare_request(&other, &cancellation, first.id);
        assert!(!a.cancellation.is_cancelled());
        h.server.prepare_request(&p, &cancellation, first.id);
        assert!(a.cancellation.is_cancelled());
        assert!(!b.cancellation.is_cancelled());
        assert!(!c.cancellation.is_cancelled());
        drop(first);
        assert!(c.cancellation.is_cancelled());
        assert!(!b.cancellation.is_cancelled());
        drop((a, b, c));
        assert!(h.server.in_flight.0.lock().unwrap().is_empty());
    }
}
