//! Magi MCP 服务的宿主适配：把 `magi-mcp-server` 的 trait 接到 Magi 的真实能力。
//!
//! - 工具执行：`magi_conversation_runtime::external_tool`（与会话内 agent 同一套判定与注册表）；
//! - 人工确认：`external_approval`（复用 `ToolApprovalRegistry` 与 `tool.approval.requested`）；
//! - 变更账本：每个外部令牌一个 `SessionKind::ExternalTool` 会话，写入类工具经其 `SnapshotSession`；
//! - 审计：发布 `mcp.tool.call` 领域事件，不含令牌原文与文件正文。
//!
//! 令牌、权限档与协议管线归 `magi-mcp-server`；本模块不复制它们的规则。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use magi_conversation_runtime::external_approval::{
    ExternalApprovalRequest, await_external_tool_approval,
};
use magi_conversation_runtime::external_tool::{ExternalToolCall, execute_external_tool_call};
use magi_core::{AccessProfile, EventId, ExecutionResultStatus, SessionId, UtcMillis, WorkspaceId};
use magi_event_bus::{EventContext, EventEnvelope};
use magi_mcp_server::server::BoxFuture;
use magi_mcp_server::{
    AttributionTarget, AuditEvent, AuditOutcome, AuditSink, InvocationOutcome, PathAccess,
    PathRequest, Profile, TokenStore, ToolBackend, ToolClass, ToolInvocation, ToolSchema,
    ToolSchemaProvider, WorkspaceResolver, mapping_for_public_name,
};
use magi_tool_runtime::{BuiltinToolName, tool_path_access_requests};
use serde_json::Value;

use crate::state::ApiState;

/// 等待 Magi 界面人工确认的默认时限。超时后调用以工具错误收口，不会稍后补执行。
pub(crate) const EXTERNAL_APPROVAL_TIMEOUT: Duration = Duration::from_secs(90);

/// 外部令牌对应的外部工具会话 id。
pub(crate) fn external_session_id(token_id: &str) -> SessionId {
    SessionId::new(format!("external-tool-{token_id}"))
}

/// Magi 内置工具的说明与 schema，MCP 目录直接使用，不另写一份。
pub(crate) struct BuiltinSchemas;

impl ToolSchemaProvider for BuiltinSchemas {
    fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
        let tool = BuiltinToolName::from_name(internal_name)?;
        if !tool.is_public_tool_surface() {
            return None;
        }
        Some(ToolSchema {
            description: tool.description().to_string(),
            input_schema: tool.parameters_schema(),
        })
    }
}

pub(crate) struct RegistryWorkspaces {
    state: ApiState,
}

impl WorkspaceResolver for RegistryWorkspaces {
    fn root_of(&self, workspace_id: &str) -> Option<PathBuf> {
        self.state
            .workspace_root_path(&Some(WorkspaceId::new(workspace_id)))
    }
}

/// 审计写入领域事件流。
pub(crate) struct EventBusAudit {
    state: ApiState,
}

impl AuditSink for EventBusAudit {
    fn record(&self, event: AuditEvent) {
        let (outcome, detail) = match &event.outcome {
            AuditOutcome::Executed { is_error } => {
                (if *is_error { "failed" } else { "succeeded" }, None)
            }
            AuditOutcome::Denied { reason } => ("denied", Some(reason.clone())),
        };
        let workspace_id = WorkspaceId::new(event.workspace_id.clone());
        let _ = self.state.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-mcp-tool-call-{}-{}",
                    event.token_id, event.at_ms
                )),
                "mcp.tool.call",
                serde_json::json!({
                    "token_id": event.token_id,
                    "client_name": event.client_name,
                    "workspace_id": event.workspace_id,
                    "tool": event.tool,
                    "requires_approval": event.requires_approval,
                    "paths": event.paths,
                    "outcome": outcome,
                    "detail": detail,
                    "at_ms": event.at_ms,
                }),
            )
            .with_context(EventContext {
                workspace_id: Some(workspace_id),
                ..EventContext::default()
            }),
        );
    }
}

/// 后端：把 MCP 调用交给 Magi 的工具执行链。
pub(crate) struct ApiToolBackend {
    state: ApiState,
    tokens: Arc<TokenStore>,
    approval_timeout: Duration,
}

impl ApiToolBackend {
    pub(crate) fn new(state: ApiState, tokens: Arc<TokenStore>) -> Self {
        Self {
            state,
            tokens,
            approval_timeout: EXTERNAL_APPROVAL_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// 令牌仍然有效（未吊销、未过期）。审批等待用它作为存活判据。
    fn token_is_active(tokens: &TokenStore, token_id: &str) -> bool {
        let now_ms = UtcMillis::now().0;
        tokens
            .records()
            .iter()
            .any(|record| record.token_id == token_id && record.is_active(now_ms))
    }

    /// 取（必要时创建）令牌自己的外部工具会话。
    fn ensure_external_session(&self, invocation: &ToolInvocation) -> Result<SessionId, String> {
        let AttributionTarget::ExternalSession { token_id } = &invocation.attribution else {
            return Err("该归属方式尚未支持".to_string());
        };
        let session_id = external_session_id(token_id);
        if self.state.session_store.session(&session_id).is_none() {
            let title = format!("{} · 外部工具", invocation.principal.client_name);
            if let Err(error) = self.state.session_store.create_external_tool_session(
                session_id.clone(),
                title,
                invocation.principal.workspace_id.clone(),
            ) {
                // 并发创建时另一方已建好，属于正常竞态。
                if self.state.session_store.session(&session_id).is_none() {
                    return Err(format!("创建外部工具会话失败: {error}"));
                }
            }
        }
        Ok(session_id)
    }
}

fn approval_summary(invocation: &ToolInvocation, root: &Path) -> String {
    let paths = invocation
        .resolved_paths
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        invocation.public_name.clone()
    } else {
        format!("{}: {}", invocation.public_name, paths.join(", "))
    }
}

fn error_outcome(text: impl Into<String>) -> InvocationOutcome {
    InvocationOutcome {
        text: text.into(),
        is_error: true,
    }
}

impl ToolBackend for ApiToolBackend {
    fn schemas(&self) -> &dyn ToolSchemaProvider {
        static SCHEMAS: BuiltinSchemas = BuiltinSchemas;
        &SCHEMAS
    }

    fn path_requests(
        &self,
        internal_name: &str,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Vec<PathRequest> {
        tool_path_access_requests(
            internal_name,
            &arguments.to_string(),
            Some(workspace_root),
            AccessProfile::Restricted,
        )
        .into_iter()
        .map(|request| PathRequest {
            raw: request.absolute_path.to_string_lossy().into_owned(),
            access: match request.kind {
                magi_permissions::PathAccessKind::Read => PathAccess::Read,
                magi_permissions::PathAccessKind::Write => PathAccess::Write,
            },
        })
        .collect()
    }

    fn invoke<'a>(&'a self, invocation: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
        Box::pin(async move {
            let Some(registry) = self.state.tool_registry().cloned() else {
                return error_outcome("工具运行时未就绪");
            };
            let session_id = match self.ensure_external_session(&invocation) {
                Ok(id) => id,
                Err(error) => return error_outcome(error),
            };
            let workspace_id = WorkspaceId::new(invocation.principal.workspace_id.clone());
            let Some(root) = self.state.workspace_root_path(&Some(workspace_id.clone())) else {
                return error_outcome("令牌绑定的工作区不可用");
            };
            let class = mapping_for_public_name(&invocation.public_name).map(|m| m.class);
            // 写入类调用才需要会话账本；只读调用不触发整树快照的懒启动。
            let snapshot = if matches!(class, Some(ToolClass::Read)) {
                None
            } else {
                match self.state.ensure_snapshot_session(&session_id, &root).await {
                    Ok(snapshot) => Some(snapshot),
                    Err(_) => return error_outcome("变更账本不可用，已拒绝执行写入类工具"),
                }
            };

            let state = self.state.clone();
            let tokens = self.tokens.clone();
            let approval_timeout = self.approval_timeout;
            let joined = tokio::task::spawn_blocking(move || {
                run_blocking(
                    &state,
                    &tokens,
                    approval_timeout,
                    &registry,
                    &invocation,
                    &session_id,
                    &workspace_id,
                    &root,
                    snapshot
                        .as_deref()
                        .map(|s| s as &dyn magi_snapshot::ToolHook),
                )
            })
            .await;
            match joined {
                Ok(outcome) => outcome,
                Err(_) => error_outcome("工具执行意外中断"),
            }
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn run_blocking(
    state: &ApiState,
    tokens: &Arc<TokenStore>,
    approval_timeout: Duration,
    registry: &magi_tool_runtime::ToolRegistry,
    invocation: &ToolInvocation,
    session_id: &SessionId,
    workspace_id: &WorkspaceId,
    root: &Path,
    snapshot: Option<&dyn magi_snapshot::ToolHook>,
) -> InvocationOutcome {
    let arguments_json = invocation.arguments.to_string();
    let call_id = format!(
        "mcp-{}-{}",
        invocation.principal.token_id,
        UtcMillis::now().0
    );
    let safety_gate =
        magi_conversation_runtime::safety_gate_from_settings(Some(&state.settings_store));
    let token_id = invocation.principal.token_id.clone();
    let is_alive = {
        let tokens = tokens.clone();
        move || ApiToolBackend::token_is_active(&tokens, &token_id)
    };
    let summary = approval_summary(invocation, root);
    let approve = |call_id: &str| {
        await_external_tool_approval(
            &state.event_bus,
            state.conversation_registry.tool_approvals(),
            &ExternalApprovalRequest {
                session_id,
                workspace_id: Some(workspace_id),
                token_id: &invocation.principal.token_id,
                client_name: &invocation.principal.client_name,
                token_prefix: &invocation.principal.token_prefix,
                tool_call_id: call_id,
                tool_name: &invocation.internal_name,
                arguments_json: &arguments_json,
                summary: &summary,
                timeout: approval_timeout,
            },
            &is_alive,
        )
    };

    let mut granted = false;
    if invocation.requires_approval {
        if let Err((payload, _)) = approve(&call_id) {
            return error_outcome(payload);
        }
        granted = true;
    }
    let access_profile = access_profile_for(invocation.principal.profile);
    let external_call = ExternalToolCall {
        call_id: &call_id,
        tool_name: &invocation.internal_name,
        arguments_json: &arguments_json,
        session_id,
        workspace_id: Some(workspace_id),
        workspace_root: root,
        access_profile,
        snapshot,
    };
    let mut result = execute_external_tool_call(
        &state.event_bus,
        registry,
        safety_gate.as_ref(),
        &external_call,
        granted,
    );
    // Magi 自己的安全规则要求授权（例如 SafetyGate 对敏感操作）时，补一次人工确认后重入。
    if result.1 == ExecutionResultStatus::NeedsApproval && !granted {
        if let Err((payload, _)) = approve(&format!("{call_id}-safety")) {
            return error_outcome(payload);
        }
        result = execute_external_tool_call(
            &state.event_bus,
            registry,
            safety_gate.as_ref(),
            &external_call,
            true,
        );
    }
    InvocationOutcome {
        is_error: result.1 != ExecutionResultStatus::Succeeded,
        text: result.0,
    }
}

/// MCP 权限档到 Magi 访问档的映射：只读档收紧为 ReadOnly；其余一律 Restricted，
/// 绝不映射到 FullAccess（shell 仍受 Magi 的命令风险判定与 SafetyGate 约束）。
fn access_profile_for(profile: Profile) -> AccessProfile {
    match profile {
        Profile::ReadOnly => AccessProfile::ReadOnly,
        Profile::Edit | Profile::EditTrusted | Profile::Exec => AccessProfile::Restricted,
    }
}

/// 组装 MCP 服务实例（协议管线 + 宿主适配）。
pub(crate) fn build_mcp_server(
    state: ApiState,
    tokens: Arc<TokenStore>,
) -> Arc<magi_mcp_server::McpServer> {
    let backend = Arc::new(ApiToolBackend::new(state.clone(), tokens));
    Arc::new(
        magi_mcp_server::McpServer::new(
            "magi",
            env!("CARGO_PKG_VERSION"),
            backend,
            Arc::new(RegistryWorkspaces {
                state: state.clone(),
            }),
        )
        .with_audit(Arc::new(EventBusAudit { state })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use magi_core::AbsolutePath;
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_mcp_server::http::{HttpConfig, HttpState, router};
    use magi_mcp_server::{AttributionMode, IssueTokenRequest};
    use magi_session_store::{SessionKind, SessionStore};
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use tower::ServiceExt;

    struct Fixture {
        state: ApiState,
        router: axum::Router,
        secret: String,
        token_id: String,
        root: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture(profile: Profile, approval_timeout: Duration) -> Fixture {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let mut registry = ToolRegistry::new(governance.clone(), event_bus.clone());
        registry.register_default_builtins();
        let state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(registry);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        state
            .workspace_registry
            .register(
                WorkspaceId::new("ws-mcp"),
                AbsolutePath::new(root.display().to_string()),
            )
            .unwrap();
        let tokens = Arc::new(TokenStore::new());
        let issued = tokens
            .issue(
                IssueTokenRequest {
                    client_name: "cursor".to_string(),
                    workspace_id: "ws-mcp".to_string(),
                    profile,
                    attribution: AttributionMode::External,
                    ttl_ms: None,
                },
                UtcMillis::now().0,
            )
            .unwrap();
        let backend = ApiToolBackend::new(state.clone(), tokens.clone())
            .with_approval_timeout(approval_timeout);
        let server = Arc::new(
            magi_mcp_server::McpServer::new(
                "magi",
                "test",
                Arc::new(backend),
                Arc::new(RegistryWorkspaces {
                    state: state.clone(),
                }),
            )
            .with_audit(Arc::new(EventBusAudit {
                state: state.clone(),
            })),
        );
        Fixture {
            state,
            router: router(HttpState::new(server, tokens, HttpConfig::default())),
            secret: issued.secret,
            token_id: issued.record.token_id,
            root,
            _dir: dir,
        }
    }

    async fn rpc(fixture: &Fixture, method: &str, params: Value) -> Value {
        let body =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let response = fixture
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::HOST, "127.0.0.1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", fixture.secret))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn call(name: &str, arguments: Value) -> Value {
        serde_json::json!({ "name": name, "arguments": arguments })
    }

    fn is_error(response: &Value) -> bool {
        response["result"]["isError"].as_bool().unwrap_or(false)
    }

    fn resolve_pending(
        state: &ApiState,
        token_id: &str,
        decision: magi_conversation_runtime::ToolApprovalDecision,
    ) -> std::thread::JoinHandle<()> {
        let state = state.clone();
        let session_id = external_session_id(token_id);
        std::thread::spawn(move || {
            let approvals = state.conversation_registry.tool_approvals().clone();
            for _ in 0..200 {
                if let Some(pending) = approvals.pending_for_session(&session_id).first() {
                    approvals
                        .resolve(&session_id, &pending.approval_id, decision)
                        .unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("待审批没有出现");
        })
    }

    #[tokio::test]
    async fn edit_profile_lists_write_tools_but_not_shell() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        let response = rpc(&f, "tools/list", serde_json::json!({})).await;
        let names = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"magi.fs.read".to_string()));
        assert!(names.contains(&"magi.fs.write".to_string()));
        assert!(!names.contains(&"magi.shell.exec".to_string()));
    }

    #[tokio::test]
    async fn read_runs_without_approval_and_paths_outside_the_workspace_are_denied() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        std::fs::write(f.root.join("hello.txt"), "hi there").unwrap();

        let ok = rpc(
            &f,
            "tools/call",
            call("magi.fs.read", serde_json::json!({"path": "hello.txt"})),
        )
        .await;
        assert!(!is_error(&ok), "{ok}");
        assert!(ok.to_string().contains("hi there"));

        for path in ["../escape.txt", "/etc/passwd", ".magi/state.json"] {
            let denied = rpc(
                &f,
                "tools/call",
                call("magi.fs.read", serde_json::json!({"path": path})),
            )
            .await;
            assert!(is_error(&denied), "{path}: {denied}");
        }
    }

    #[tokio::test]
    async fn approved_write_lands_in_the_external_session_ledger() {
        let f = fixture(Profile::Edit, Duration::from_secs(10));
        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let response = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "new.txt", "content": "from mcp"}),
            ),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&response), "{response}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("new.txt")).unwrap(),
            "from mcp"
        );

        let session = f
            .state
            .session_store
            .session(&external_session_id(&f.token_id))
            .expect("外部工具会话应已创建");
        assert_eq!(session.kind, SessionKind::ExternalTool);
        assert!(
            f.state.session_store.current_session().is_none(),
            "外部会话不得成为用户当前会话"
        );
        let snapshot = f
            .state
            .snapshot_session(&external_session_id(&f.token_id), &f.root)
            .expect("写入类调用应建立账本");
        snapshot.reconcile().unwrap();
        let pending = snapshot.pending_changes().unwrap();
        assert!(
            pending
                .iter()
                .any(|change| change.path.ends_with("new.txt")),
            "写入应出现在待处理变更里: {pending:?}"
        );
    }

    #[tokio::test]
    async fn denied_or_timed_out_write_leaves_the_file_untouched() {
        let f = fixture(Profile::Edit, Duration::from_millis(400));
        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::Deny,
        );
        let denied = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "a.txt", "content": "x"}),
            ),
        )
        .await;
        approver.join().unwrap();
        assert!(is_error(&denied), "{denied}");
        assert!(!f.root.join("a.txt").exists());

        let timed_out = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "b.txt", "content": "x"}),
            ),
        )
        .await;
        assert!(is_error(&timed_out), "{timed_out}");
        assert!(timed_out.to_string().contains("tool_approval_timeout"));
        assert!(!f.root.join("b.txt").exists());
    }

    #[tokio::test]
    async fn read_only_profile_never_sees_or_runs_write_tools() {
        let f = fixture(Profile::ReadOnly, Duration::from_secs(5));
        let response = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "a.txt", "content": "x"}),
            ),
        )
        .await;
        assert!(is_error(&response));
        assert!(!f.root.join("a.txt").exists());
    }
}
