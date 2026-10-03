//! MCP 请求分发与调用管线。
//!
//! 服务是**薄的协议与策略层**：不复制工具实现，也不持有审批与账本。一次 `tools/call`
//! 的顺序固定为：
//!
//! 工具是否在该权限档的目录里 → 归属解析 → 工作区路径校验 → 审批处置 → 后端执行 → 审计。
//!
//! 任何一步失败都以 MCP 工具错误返回，后续步骤不执行；网络与本机客户端走同一条管线。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::catalog::{
    DynamicTool, ResolvedTool, ToolDescriptor, ToolSchemaProvider, build_catalog,
    mapping_for_public_name, tools_list_result,
};
use crate::path_guard::{PathGuard, PathRequest};
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
    pub attribution: AttributionMode,
}

impl From<&TokenRecord> for ClientPrincipal {
    fn from(record: &TokenRecord) -> Self {
        Self {
            token_id: record.token_id.clone(),
            token_prefix: record.prefix.clone(),
            client_name: record.client_name.clone(),
            workspace_id: record.workspace_id.clone(),
            profile: record.profile,
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

pub struct McpServer {
    name: String,
    version: String,
    backend: Arc<dyn ToolBackend>,
    workspaces: Arc<dyn WorkspaceResolver>,
    attribution: Arc<dyn AttributionResolver>,
    audit: Arc<dyn AuditSink>,
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
        if !request.is_well_formed() {
            return Some(error(
                request.response_id(),
                JSONRPC_INVALID_REQUEST,
                "无效的 JSON-RPC 请求",
            ));
        }
        if request.is_notification() {
            // 通知（如 notifications/initialized、notifications/cancelled）不产生响应。
            return None;
        }
        let id = request.response_id();
        match request.method.as_str() {
            "initialize" => Some(success(id, initialize_result(&self.name, &self.version))),
            "ping" => Some(success(id, ping_result())),
            "tools/list" => Some(success(id, tools_list_result(&self.catalog_for(principal)))),
            "tools/call" => {
                let params = request.params.unwrap_or(Value::Null);
                match self.call_tool(principal, &params, now_ms).await {
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

        let requires_approval = requires_approval(principal.profile, &mapping);
        let invocation = ToolInvocation {
            principal: principal.clone(),
            public_name: mapping.public_name.clone(),
            internal_name: mapping.internal_name.clone(),
            class: mapping.class,
            arguments,
            requires_approval,
            attribution,
            resolved_paths: resolved_paths.clone(),
        };
        let outcome = self.backend.invoke(invocation).await;

        self.audit.record(AuditEvent {
            token_id: principal.token_id.clone(),
            client_name: principal.client_name.clone(),
            workspace_id: principal.workspace_id.clone(),
            tool: mapping.public_name.clone(),
            requires_approval,
            paths: paths_summary(&resolved_paths),
            outcome: AuditOutcome::Executed {
                is_error: outcome.is_error,
            },
            at_ms: now_ms,
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
    async fn read_tools_run_without_approval_and_writes_require_it() {
        let h = harness();
        let read = h
            .server
            .handle(
                &principal(Profile::Edit),
                call("magi.fs.read", json!({"path": "a.txt"})),
                5,
            )
            .await
            .unwrap();
        assert!(!is_error(&read));
        let write = h
            .server
            .handle(
                &principal(Profile::Edit),
                call("magi.fs.write", json!({"path": "b.txt", "content": "x"})),
                6,
            )
            .await
            .unwrap();
        assert!(!is_error(&write));

        let calls = h.backend.calls.lock().unwrap();
        assert!(!calls[0].requires_approval);
        assert!(calls[1].requires_approval);
        assert_eq!(calls[1].internal_name, "file_write");
        assert_eq!(
            calls[1].attribution,
            AttributionTarget::ExternalSession {
                token_id: "tok-1".to_string()
            }
        );
        drop(calls);

        let events = h.audit.0.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].tool, "magi.fs.write");
        assert!(events[1].requires_approval);
    }

    #[tokio::test]
    async fn trusted_profile_skips_approval_for_writes_but_never_for_remove() {
        let h = harness();
        let p = principal(Profile::EditTrusted);
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
        let calls = h.backend.calls.lock().unwrap();
        assert!(!calls[0].requires_approval);
        assert!(calls[1].requires_approval, "破坏性工具不受预授权豁免");
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
}
