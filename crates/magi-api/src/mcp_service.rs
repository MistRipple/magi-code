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
    AttributionTarget, AuditEvent, AuditOutcome, AuditSink, DynamicTool, InvocationOutcome,
    PathAccess, PathRequest, Profile, TokenStore, ToolBackend, ToolClass, ToolInvocation,
    ToolSchema, ToolSchemaProvider, WorkspaceResolver,
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

/// 由宿主直接实现（不是内置工具）的变更账本工具。
const CHANGES_LIST: &str = "changes_list";
const CHANGES_REVERT: &str = "changes_revert";
/// 列表最多返回的变更条数，避免超大结果。
const CHANGES_LIST_LIMIT: usize = 500;

/// Magi 内置工具的说明与 schema，MCP 目录直接使用，不另写一份。
pub(crate) struct BuiltinSchemas;

impl ToolSchemaProvider for BuiltinSchemas {
    fn schema_for(&self, internal_name: &str) -> Option<ToolSchema> {
        match internal_name {
            CHANGES_LIST => {
                return Some(ToolSchema {
                    description: "列出该客户端在 Magi 中产生的待处理变更（路径、变更类型、大小、是否可回退）。"
                        .to_string(),
                    input_schema: serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                });
            }
            CHANGES_REVERT => {
                return Some(ToolSchema {
                    description: "回退该客户端产生的指定待处理变更，把文件恢复到变更前的状态。需要用户在 Magi 中确认。"
                        .to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "paths": {
                                "type": "array",
                                "items": { "type": "string" },
                                "minItems": 1,
                                "description": "要回退的文件路径（相对工作区根）"
                            }
                        },
                        "required": ["paths"],
                        "additionalProperties": false
                    }),
                });
            }
            _ => {}
        }
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
        self.state
            .mcp_service
            .audit()
            .record(crate::mcp_runtime::AuditEntry {
                token_id: event.token_id.clone(),
                client_name: event.client_name.clone(),
                workspace_id: event.workspace_id.clone(),
                tool: event.tool.clone(),
                requires_approval: event.requires_approval,
                paths: event.paths.clone(),
                outcome: outcome.to_string(),
                detail: detail.clone(),
                at_ms: event.at_ms,
            });
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

/// 内置工具在网关里的权限类别：只读自动放行；写入需确认；删除、推送与未知副作用永远逐次确认；
/// 命令执行只对 Exec 档开放。
fn builtin_tool_class(tool: BuiltinToolName) -> ToolClass {
    match tool {
        BuiltinToolName::ShellExec
        | BuiltinToolName::ProcessLaunch
        | BuiltinToolName::ProcessWrite => ToolClass::Exec,
        BuiltinToolName::FileRemove
        | BuiltinToolName::ProcessKill
        | BuiltinToolName::GitPush
        | BuiltinToolName::GitBranchDelete
        | BuiltinToolName::GitWorktreeRemove => ToolClass::Destructive,
        other => match other.default_access_mode() {
            magi_tool_runtime::BuiltinToolAccessMode::ReadOnly => ToolClass::Read,
            _ => ToolClass::Write,
        },
    }
}

/// 网关的动态目录：项目允许的其余内置工具、已连接的下游 MCP 工具、可用的 Skill handler。
///
/// 目录直接来自工具注册表（与 Magi 自己的 agent 看到的同一份事实源），不另存一份。
/// 静态目录（`magi.fs.*`、`magi.search.*`、`magi.git.*`、`magi.changes.*`）里已有的内置工具不重复出现。
/// 内置工具的对外名字：首段当命名空间（`git_branch_create` → `magi.git.branch_create`），
/// 和静态目录里的 `magi.git.status`、`magi.search.text` 同一种形状。
fn dynamic_public_name(internal: &str) -> String {
    match internal.split_once('_') {
        Some((namespace, rest)) => format!("magi.{namespace}.{rest}"),
        None => format!("magi.{internal}"),
    }
}

fn gateway_dynamic_tools(registry: &magi_tool_runtime::ToolRegistry) -> Vec<DynamicTool> {
    let static_internal = magi_mcp_server::V1_TOOLS
        .iter()
        .map(|mapping| mapping.internal_name)
        .collect::<std::collections::HashSet<_>>();
    let mut tools = Vec::new();
    for spec in registry.public_builtin_specs() {
        let Some(tool) = BuiltinToolName::from_name(&spec.name) else {
            continue;
        };
        if static_internal.contains(tool.as_str())
            || !magi_conversation_runtime::external_tool::builtin_exposed_externally(tool)
        {
            continue;
        }
        tools.push(DynamicTool {
            public_name: dynamic_public_name(tool.as_str()),
            internal_name: tool.as_str().to_string(),
            class: builtin_tool_class(tool),
            description: tool.description().to_string(),
            input_schema: tool.parameters_schema(),
        });
    }
    let external = registry.external_tool_catalog_snapshot();
    let connected = external
        .mcp_servers
        .iter()
        .filter(|server| server.enabled && server.connected)
        .map(|server| server.server_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    for tool in &external.mcp_tools {
        if !connected.contains(tool.server_id.as_str()) {
            continue;
        }
        tools.push(DynamicTool {
            public_name: format!("mcp.{}", tool.model_tool_name),
            internal_name: tool.model_tool_name.clone(),
            // 下游工具的副作用 Magi 看不见：只有它自己声明只读才自动放行，其余永远逐次确认。
            class: if tool.read_only {
                ToolClass::Read
            } else {
                ToolClass::Destructive
            },
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        });
    }
    for tool in external
        .skill_tools
        .iter()
        .filter(|tool| tool.status == "available")
    {
        tools.push(DynamicTool {
            public_name: format!("skill.{}", tool.name),
            internal_name: tool.name.clone(),
            class: ToolClass::Destructive,
            description: tool.description.clone(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "payload": { "type": "string", "description": "传给该 Skill handler 的原始输入" } },
                "required": ["payload"]
            }),
        });
    }
    tools.sort_by(|left, right| left.public_name.cmp(&right.public_name));
    tools.dedup_by(|left, right| left.public_name == right.public_name);
    tools
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

    /// 变更账本工具：直接操作该外部会话的 `SnapshotSession`。
    async fn run_changes_tool(
        &self,
        invocation: &ToolInvocation,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        root: &Path,
    ) -> InvocationOutcome {
        let snapshot = match self.state.ensure_snapshot_session(session_id, root).await {
            Ok(snapshot) => snapshot,
            Err(_) => return error_outcome("变更账本不可用"),
        };
        match invocation.internal_name.as_str() {
            CHANGES_LIST => {
                let listing = tokio::task::spawn_blocking(move || {
                    snapshot
                        .reconcile()
                        .and_then(|()| snapshot.pending_changes())
                })
                .await;
                let changes = match listing {
                    Ok(Ok(changes)) => changes,
                    _ => return error_outcome("读取变更列表失败"),
                };
                let total = changes.len();
                let items = changes
                    .iter()
                    .take(CHANGES_LIST_LIMIT)
                    .map(|change| {
                        serde_json::json!({
                            "path": change.path,
                            "changeKind": change.change_kind,
                            "size": change.size,
                            "revertible": change.revertible,
                        })
                    })
                    .collect::<Vec<_>>();
                InvocationOutcome {
                    text: serde_json::json!({
                        "total": total,
                        "truncated": total > CHANGES_LIST_LIMIT,
                        "changes": items,
                    })
                    .to_string(),
                    is_error: false,
                }
            }
            CHANGES_REVERT => {
                let paths = revert_paths(&invocation.arguments);
                if paths.is_empty() {
                    return error_outcome("paths 不能为空");
                }
                // 先查冲突再请求审批：不要让用户批准一个注定被拒绝的回退。
                match check_revert_conflicts(snapshot.clone(), paths.clone()).await {
                    Ok(None) => {}
                    Ok(Some(message)) => return error_outcome(message),
                    Err(message) => return error_outcome(message),
                }
                if invocation.requires_approval {
                    let state = self.state.clone();
                    let tokens = self.tokens.clone();
                    let timeout = self.approval_timeout;
                    let invocation = invocation.clone();
                    let session_id = session_id.clone();
                    let workspace_id = workspace_id.clone();
                    let root = root.to_path_buf();
                    let approved = tokio::task::spawn_blocking(move || {
                        approve_blocking(
                            &state,
                            &tokens,
                            timeout,
                            &invocation,
                            &session_id,
                            &workspace_id,
                            &root,
                        )
                    })
                    .await;
                    match approved {
                        Ok(Ok(())) => {}
                        Ok(Err(payload)) => return error_outcome(payload),
                        Err(_) => return error_outcome("审批等待意外中断"),
                    }
                }
                // 审批等待期间文件可能又被改过，落盘前再确认一次。
                match check_revert_conflicts(snapshot.clone(), paths.clone()).await {
                    Ok(None) => {}
                    Ok(Some(message)) => return error_outcome(message),
                    Err(message) => return error_outcome(message),
                }
                let reverted = tokio::task::spawn_blocking(move || snapshot.revert(&paths)).await;
                match reverted {
                    Ok(Ok(count)) => InvocationOutcome {
                        text: serde_json::json!({ "reverted": count }).to_string(),
                        is_error: false,
                    },
                    Ok(Err(error)) => error_outcome(format!("回退失败: {error}")),
                    Err(_) => error_outcome("回退失败"),
                }
            }
            _ => error_outcome("未知的变更工具"),
        }
    }

    /// 调用归属的会话：外部令牌取（必要时创建）自己的外部工具会话；
    /// 槽位令牌取槽位拥有者的会话，并确认该 turn 仍在进行。
    fn resolve_session(&self, invocation: &ToolInvocation) -> Result<SessionId, String> {
        match &invocation.attribution {
            AttributionTarget::ExternalSession { token_id } => {
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
            AttributionTarget::WebSlotTurn {
                session_id,
                turn_id,
            } => {
                if !slot_turn_is_active(&self.state, turn_id) {
                    return Err("当前没有可归属的进行中的 GPT Web 对话".to_string());
                }
                let session_id = SessionId::new(session_id.clone());
                if self.state.session_store.session(&session_id).is_none() {
                    return Err("槽位拥有者会话不存在".to_string());
                }
                Ok(session_id)
            }
        }
    }
}

/// 槽位表里当前进行中的 Web turn 是否就是这一个。槽位表是唯一事实源。
fn slot_turn_is_active(state: &ApiState, turn_id: &str) -> bool {
    state
        .web_model
        .bindings()
        .and_then(|slots| slots.active_context())
        .is_some_and(|(_, active)| active.to_string() == turn_id)
}

/// 一个“可能产生外部待审批”的调用方：令牌，或 GPT Web 槽位端点。
pub(crate) struct ApprovalOwner {
    pub token_id: String,
    pub client_name: String,
    pub token_prefix: String,
    pub workspace_id: String,
    /// 待审批记录挂在哪个会话上：令牌的外部工具会话，或槽位拥有者的会话。
    pub session_id: SessionId,
}

/// 当前所有可能持有外部待审批的调用方。跨会话待确认入口只扫描它们。
pub(crate) fn approval_owners(state: &ApiState) -> Vec<ApprovalOwner> {
    let mut owners = state
        .mcp_service
        .list_tokens()
        .into_iter()
        .map(|record| ApprovalOwner {
            session_id: external_session_id(&record.token_id),
            token_id: record.token_id,
            client_name: record.client_name,
            token_prefix: record.prefix,
            workspace_id: record.workspace_id,
        })
        .collect::<Vec<_>>();
    if let Some(snapshot) = state
        .web_model
        .bindings()
        .and_then(|slots| slots.snapshot())
    {
        owners.push(ApprovalOwner {
            token_id: crate::web_slot_mcp::WEB_SLOT_PRINCIPAL_ID.to_string(),
            client_name: crate::web_slot_mcp::WEB_SLOT_CLIENT_NAME.to_string(),
            token_prefix: crate::web_slot_mcp::WEB_SLOT_PRINCIPAL_ID.to_string(),
            workspace_id: snapshot.owner_project_id,
            session_id: SessionId::new(snapshot.owner_session_id),
        });
    }
    owners
}

impl ApiState {
    /// 装配槽位表的 turn 结束 / 释放回调（daemon 装配槽位表之后调用）。
    pub fn install_web_slot_hooks(&self) {
        install_web_slot_end_hook(self);
    }
}

/// 槽位拥有者 turn 结束或槽位释放时，取消该槽位遗留的待审批（等待方以“已取消”收口，不补执行）。
pub(crate) fn install_web_slot_end_hook(state: &ApiState) {
    let Some(slots) = state.web_model.bindings() else {
        return;
    };
    let approvals = state.conversation_registry.tool_approvals().clone();
    slots.set_end_hook(Some(Arc::new(
        move |owner: &magi_web_model::WebSlotOwner| {
            magi_conversation_runtime::external_approval::cancel_external_approvals(
                &approvals,
                &SessionId::new(owner.session_id.clone()),
                crate::web_slot_mcp::WEB_SLOT_PRINCIPAL_ID,
            );
        },
    )));
}

/// 审批等待的存活判据：外部令牌要求令牌有效；槽位调用要求它归属的 Web turn 仍在进行
/// （槽位释放或 turn 结束都会使它失效）。
fn liveness(
    state: &ApiState,
    tokens: &Arc<TokenStore>,
    invocation: &ToolInvocation,
) -> impl Fn() -> bool + Send + 'static {
    let tokens = tokens.clone();
    let state = state.clone();
    let token_id = invocation.principal.token_id.clone();
    let attribution = invocation.attribution.clone();
    move || match &attribution {
        AttributionTarget::WebSlotTurn { turn_id, .. } => slot_turn_is_active(&state, turn_id),
        AttributionTarget::ExternalSession { .. } => {
            ApiToolBackend::token_is_active(&tokens, &token_id)
        }
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

/// 回退前检查：这些文件在本会话最后一次工具写入之后有没有被别的来源（用户或其他客户端）改过。
/// 有则拒绝，避免把别人后来的修改静默抹掉；`Ok(None)` 表示可以回退。
async fn check_revert_conflicts(
    snapshot: Arc<magi_snapshot::SnapshotSession>,
    paths: Vec<String>,
) -> Result<Option<String>, String> {
    let modified =
        tokio::task::spawn_blocking(move || snapshot.paths_modified_outside_tools(&paths))
            .await
            .map_err(|_| "回退检查意外中断".to_string())?
            .map_err(|error| format!("回退检查失败: {error}"))?;
    if modified.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "拒绝回退：这些文件在该客户端最后一次写入之后又被其他来源（用户或其他客户端）修改过，回退会抹掉那些修改：{}。请先用 magi.fs.read 查看现状，确认后手动处理。",
        modified.join("、")
    )))
}

fn revert_paths(arguments: &Value) -> Vec<String> {
    arguments
        .get("paths")
        .and_then(Value::as_array)
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
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

    fn dynamic_tools(&self) -> Vec<DynamicTool> {
        self.state
            .tool_registry()
            .map(gateway_dynamic_tools)
            .unwrap_or_default()
    }

    fn path_requests(
        &self,
        internal_name: &str,
        arguments: &Value,
        workspace_root: &Path,
    ) -> Vec<PathRequest> {
        if internal_name == CHANGES_REVERT {
            return revert_paths(arguments)
                .into_iter()
                .map(|raw| PathRequest {
                    raw,
                    access: PathAccess::Write,
                })
                .collect();
        }
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
            // 槽位调用：作为普通工具项内联写进 Web 会话当前 turn 的 canonical（§8.4）。
            let slot_session = match &invocation.attribution {
                AttributionTarget::WebSlotTurn { session_id, .. } => {
                    Some(SessionId::new(session_id.clone()))
                }
                AttributionTarget::ExternalSession { .. } => None,
            };
            let call_id = format!(
                "mcp-{}-{}",
                invocation.principal.token_id,
                UtcMillis::now().0
            );
            let arguments_json = invocation.arguments.to_string();
            let tool_name = invocation.public_name.clone();
            let workspace_id = Some(WorkspaceId::new(invocation.principal.workspace_id.clone()));
            let writer = slot_session.as_ref().map(|session_id| {
                (
                    session_id.clone(),
                    magi_conversation_runtime::session_writeback::ExternalToolItemWriter {
                        session_store: &self.state.session_store,
                        event_bus: &self.state.event_bus,
                        session_id,
                        workspace_id: &workspace_id,
                    },
                )
            });
            if let Some((_, writer)) = &writer
                && let Err(error) = writer.started(&call_id, &tool_name, &arguments_json)
            {
                return error_outcome(format!("无法记录到当前 Web 对话：{error}"));
            }
            let outcome = self.invoke_resolved(invocation).await;
            if let Some((_, writer)) = &writer {
                let status = if outcome.is_error {
                    ExecutionResultStatus::Failed
                } else {
                    ExecutionResultStatus::Succeeded
                };
                if let Err(error) =
                    writer.finished(&call_id, &tool_name, &arguments_json, &outcome.text, status)
                {
                    tracing::warn!(%error, "槽位工具结果写入 canonical 失败");
                }
            }
            outcome
        })
    }
}

impl ApiToolBackend {
    async fn invoke_resolved(&self, invocation: ToolInvocation) -> InvocationOutcome {
        {
            let Some(registry) = self.state.tool_registry().cloned() else {
                return error_outcome("工具运行时未就绪");
            };
            let session_id = match self.resolve_session(&invocation) {
                Ok(id) => id,
                Err(error) => return error_outcome(error),
            };
            let workspace_id = WorkspaceId::new(invocation.principal.workspace_id.clone());
            let Some(root) = self.state.workspace_root_path(&Some(workspace_id.clone())) else {
                return error_outcome("令牌绑定的工作区不可用");
            };
            let class = Some(invocation.class);
            if invocation.internal_name.starts_with("changes_") {
                return self
                    .run_changes_tool(&invocation, &session_id, &workspace_id, &root)
                    .await;
            }
            // git 工具需要会话的 Git context；由变更面板同一入口建立（幂等）。
            if invocation.internal_name.starts_with("git_")
                && let Err(error) = self
                    .state
                    .synchronize_session_changes(&session_id, &workspace_id, &root, false)
                    .await
            {
                return error_outcome(format!("无法建立 Git 上下文: {}", error.message()));
            }
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
        }
    }
}

/// GPT Web 槽位发起的调用按设置里的授权方式处理；其他来源（外部令牌）一律每次询问。
fn slot_approval_mode(
    state: &ApiState,
    invocation: &ToolInvocation,
) -> crate::web_model_channel::WebApprovalMode {
    if matches!(
        invocation.attribution,
        AttributionTarget::WebSlotTurn { .. }
    ) {
        effective_approval_mode(state.web_model.approval_mode(), invocation.class)
    } else {
        crate::web_model_channel::WebApprovalMode::Ask
    }
}

/// 「始终授权」只覆盖创建 / 修改类（`Write`）。删除、推送、合并等破坏性操作和命令执行
/// （`Destructive` / `Exec`）按权限档的约定永远逐次确认，不能被一个全局开关静默放行。
fn effective_approval_mode(
    mode: crate::web_model_channel::WebApprovalMode,
    class: magi_mcp_server::ToolClass,
) -> crate::web_model_channel::WebApprovalMode {
    use crate::web_model_channel::WebApprovalMode;
    match (mode, class) {
        (
            WebApprovalMode::Always,
            magi_mcp_server::ToolClass::Destructive | magi_mcp_server::ToolClass::Exec,
        ) => WebApprovalMode::Ask,
        (mode, _) => mode,
    }
}

const WEB_APPROVAL_DENIED_BY_SETTING: &str =
    "GPT Web 的授权方式设置为「拒绝」：需要授权的操作一律不执行。";

/// 发起人工确认并阻塞等待；`Err` 携带终态结果文本。
fn approve_blocking(
    state: &ApiState,
    tokens: &Arc<TokenStore>,
    timeout: Duration,
    invocation: &ToolInvocation,
    session_id: &SessionId,
    workspace_id: &WorkspaceId,
    root: &Path,
) -> Result<(), String> {
    match slot_approval_mode(state, invocation) {
        crate::web_model_channel::WebApprovalMode::Always => return Ok(()),
        crate::web_model_channel::WebApprovalMode::Deny => {
            return Err(WEB_APPROVAL_DENIED_BY_SETTING.to_string());
        }
        crate::web_model_channel::WebApprovalMode::Ask => {}
    }
    let call_id = format!(
        "mcp-{}-{}",
        invocation.principal.token_id,
        UtcMillis::now().0
    );
    let is_alive = liveness(state, tokens, invocation);
    await_external_tool_approval(
        &state.event_bus,
        state.conversation_registry.tool_approvals(),
        &ExternalApprovalRequest {
            session_id,
            workspace_id: Some(workspace_id),
            token_id: &invocation.principal.token_id,
            client_name: &invocation.principal.client_name,
            token_prefix: &invocation.principal.token_prefix,
            tool_call_id: &call_id,
            tool_name: &invocation.internal_name,
            arguments_json: &invocation.arguments.to_string(),
            summary: &approval_summary(invocation, root),
            timeout,
        },
        &is_alive,
    )
    .map_err(|(payload, _)| payload)
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
    let is_alive = liveness(state, tokens, invocation);
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
        match slot_approval_mode(state, invocation) {
            // 「始终授权」只免掉按权限档需要的那次人工确认；工作区路径范围、审计和下面
            // SafetyGate 对敏感操作的确认都照常生效。
            crate::web_model_channel::WebApprovalMode::Always => {}
            crate::web_model_channel::WebApprovalMode::Deny => {
                return error_outcome(WEB_APPROVAL_DENIED_BY_SETTING);
            }
            crate::web_model_channel::WebApprovalMode::Ask => {
                if let Err((payload, _)) = approve(&call_id) {
                    return error_outcome(payload);
                }
            }
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
        skill_runtime: state.skill_runtime.as_deref(),
        skill_dispatch_runtime: state.skill_dispatch_runtime.as_deref(),
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
        if slot_approval_mode(state, invocation) == crate::web_model_channel::WebApprovalMode::Deny
        {
            return error_outcome(WEB_APPROVAL_DENIED_BY_SETTING);
        }
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

fn slots_provider(
    state: &ApiState,
) -> Arc<dyn Fn() -> Option<Arc<magi_web_model::WebSlotTable>> + Send + Sync> {
    let harness = state.web_model.clone();
    Arc::new(move || harness.bindings())
}

/// GPT Web 连接器当前从 Magi MCP 目录可见的工具数。
///
/// ChatGPT 的已安装连接器行不稳定地展示工具数；此处复用槽位端点的身份和工具目录，
/// 返回 Magi 实际通过 `tools/list` 暴露的数量。
pub(crate) fn web_slot_tool_count(state: &ApiState) -> Option<u64> {
    u64::try_from(web_slot_catalog(state)?.len()).ok()
}

/// GPT Web 连接器目录的指纹：工具名、描述与输入 schema 的 SHA-256（按名称排序）。
///
/// ChatGPT 会缓存创建 / 上次刷新时的工具列表，Magi 目录变化后不会自动同步；
/// 记下上次推给 ChatGPT 时的指纹，就能知道它手里的列表是不是已经过期。
pub(crate) fn web_slot_tool_digest(state: &ApiState) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut tools = web_slot_catalog(state)?;
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let mut hasher = Sha256::new();
    for tool in tools {
        hasher.update(tool.name.as_bytes());
        hasher.update([0]);
        hasher.update(tool.description.as_bytes());
        hasher.update([0]);
        hasher.update(tool.input_schema.to_string().as_bytes());
        hasher.update([1]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn web_slot_catalog(state: &ApiState) -> Option<Vec<magi_mcp_server::ToolDescriptor>> {
    let harness = state.web_model.clone();
    let auth = crate::web_slot_mcp::slot_principal_provider(
        slots_provider(state),
        Arc::new(move || {
            crate::web_slot_mcp::slot_profile_from_setting(
                harness
                    .configured()
                    .and_then(|config| config.tool_profile)
                    .as_deref(),
            )
        }),
    );
    let magi_mcp_server::local_socket::SocketAuth::Principal(provider) = auth else {
        return None;
    };
    let principal = provider()?;
    let server = build_slot_mcp_server(state.clone());
    Some(server.catalog_for(&principal))
}

fn assemble_server(
    state: ApiState,
    backend: Arc<ApiToolBackend>,
) -> Arc<magi_mcp_server::McpServer> {
    Arc::new(
        magi_mcp_server::McpServer::new(
            "magi",
            env!("CARGO_PKG_VERSION"),
            backend,
            Arc::new(RegistryWorkspaces {
                state: state.clone(),
            }),
        )
        .with_attribution(Arc::new(crate::web_slot_mcp::SlotAttribution {
            slots: slots_provider(&state),
        }))
        .with_audit(Arc::new(EventBusAudit { state })),
    )
}

/// 组装令牌端点（回环 HTTP / stdio）的 MCP 服务实例（协议管线 + 宿主适配）。
pub(crate) fn build_mcp_server(
    state: ApiState,
    tokens: Arc<TokenStore>,
) -> Arc<magi_mcp_server::McpServer> {
    let backend = Arc::new(ApiToolBackend::new(state.clone(), tokens));
    assemble_server(state, backend)
}

/// 组装 GPT Web 槽位端点的 MCP 服务实例。槽位调用的身份与归属来自槽位表，不使用令牌。
pub(crate) fn build_slot_mcp_server(state: ApiState) -> Arc<magi_mcp_server::McpServer> {
    build_mcp_server(state, Arc::new(TokenStore::new()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dynamic_builtin_tools_share_the_namespace_shape_of_the_static_catalog() {
        assert_eq!(
            super::dynamic_public_name("git_branch_create"),
            "magi.git.branch_create"
        );
        assert_eq!(super::dynamic_public_name("web_search"), "magi.web.search");
        assert_eq!(
            super::dynamic_public_name("knowledge_graph_query"),
            "magi.knowledge.graph_query"
        );
    }

    #[test]
    fn always_approval_never_covers_destructive_or_exec_tools() {
        use crate::web_model_channel::WebApprovalMode::{Always, Ask, Deny};
        assert_eq!(effective_approval_mode(Always, ToolClass::Write), Always);
        assert_eq!(effective_approval_mode(Always, ToolClass::Read), Always);
        assert_eq!(effective_approval_mode(Always, ToolClass::Destructive), Ask);
        assert_eq!(effective_approval_mode(Always, ToolClass::Exec), Ask);
        assert_eq!(effective_approval_mode(Deny, ToolClass::Destructive), Deny);
        assert_eq!(effective_approval_mode(Ask, ToolClass::Write), Ask);
    }

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
        mcp_calls: Arc<std::sync::atomic::AtomicUsize>,
        _dir: tempfile::TempDir,
    }

    fn fixture(profile: Profile, approval_timeout: Duration) -> Fixture {
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let base = ApiState::new(
            "magi-test",
            event_bus.clone(),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance.clone(),
        );
        let state_dir = tempfile::tempdir().unwrap();
        let git_executor = crate::git_tool_runtime::build_git_tool_executor(
            crate::git_tool_runtime::GitToolRuntimeDependencies {
                git_service: base.git_service.clone(),
                session_code_contexts: base.session_code_contexts.clone(),
                workspace_git_coordinator: base.workspace_git_coordinator.clone(),
                event_bus: event_bus.clone(),
                knowledge_store: base.knowledge_store.clone(),
                snapshot_manager: base.snapshot_manager.clone(),
                runtime_persistence: Arc::new(crate::RuntimeStatePersistence::new(
                    state_dir.path().join("state"),
                    state_dir.path().join("state/workspaces.json"),
                    state_dir.path().join("state/knowledge.json"),
                )),
                managed_worktree_root: state_dir.path().join("worktrees"),
            },
        );
        let mcp_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executor_calls = mcp_calls.clone();
        let mut registry = ToolRegistry::new(governance.clone(), event_bus.clone())
            .with_git_tool_executor(git_executor)
            .with_external_tool_catalog_provider(Arc::new(|| {
                use magi_tool_runtime::{
                    ExternalMcpServerCatalogEntry, ExternalMcpToolCatalogEntry,
                    ExternalToolCatalogSnapshot,
                };
                let tool = |name: &str, read_only: bool| ExternalMcpToolCatalogEntry {
                    server_id: "docs".to_string(),
                    server_name: "Docs".to_string(),
                    model_tool_name: format!("mcp__docs__{name}"),
                    tool_name: name.to_string(),
                    description: format!("{name} 下游工具"),
                    read_only,
                    input_schema: serde_json::json!({"type": "object"}),
                };
                ExternalToolCatalogSnapshot {
                    mcp_servers: vec![ExternalMcpServerCatalogEntry {
                        server_id: "docs".to_string(),
                        name: "Docs".to_string(),
                        enabled: true,
                        connected: true,
                        health: "ok".to_string(),
                        tool_count: Some(2),
                        error: None,
                    }],
                    mcp_tools: vec![tool("search", true), tool("publish", false)],
                    ..Default::default()
                }
            }))
            .with_external_mcp_tool_executor(Arc::new(move |_server, tool, _args| {
                executor_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    serde_json::json!({"status": "succeeded", "ran": tool}).to_string(),
                    magi_core::ExecutionResultStatus::Succeeded,
                )
            }));
        registry.register_default_builtins();
        std::mem::forget(state_dir);
        let state = base.with_tool_registry(registry);
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
                    network: false,
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
            mcp_calls,
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

    #[tokio::test]
    async fn changes_list_shows_own_writes_and_revert_restores_after_approval() {
        let f = fixture(Profile::Edit, Duration::from_secs(10));
        std::fs::write(f.root.join("keep.txt"), "original").unwrap();

        // 先让 snapshot 建立基线：一次经批准的覆盖写入。
        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let written = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "keep.txt", "content": "changed"}),
            ),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&written), "{written}");

        let listed = rpc(
            &f,
            "tools/call",
            call("magi.changes.list", serde_json::json!({})),
        )
        .await;
        assert!(!is_error(&listed), "{listed}");
        assert!(listed.to_string().contains("keep.txt"), "{listed}");

        // 回退是写入类：需要确认；批准后文件恢复。
        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let reverted = rpc(
            &f,
            "tools/call",
            call(
                "magi.changes.revert",
                serde_json::json!({"paths": ["keep.txt"]}),
            ),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&reverted), "{reverted}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("keep.txt")).unwrap(),
            "original"
        );
    }

    #[tokio::test]
    async fn revert_is_refused_when_someone_else_changed_the_file_after_this_client() {
        let f = fixture(Profile::Edit, Duration::from_secs(10));
        std::fs::write(f.root.join("shared.txt"), "original").unwrap();

        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let written = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "shared.txt", "content": "client A was here"}),
            ),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&written), "{written}");

        // 另一个客户端（或用户）随后改了同一个文件。
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(f.root.join("shared.txt"), "client B overwrote this, longer").unwrap();

        // 在检查阶段就被拒绝，不需要审批：没有挂起的审批可供处理。
        let reverted = rpc(
            &f,
            "tools/call",
            call(
                "magi.changes.revert",
                serde_json::json!({"paths": ["shared.txt"]}),
            ),
        )
        .await;
        assert!(is_error(&reverted), "{reverted}");
        assert!(reverted.to_string().contains("拒绝回退"), "{reverted}");
        assert!(reverted.to_string().contains("shared.txt"), "{reverted}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("shared.txt")).unwrap(),
            "client B overwrote this, longer",
            "别人的修改不能被回退抹掉"
        );
    }

    #[tokio::test]
    async fn revert_outside_the_workspace_and_approve_are_not_available() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        let escaped = rpc(
            &f,
            "tools/call",
            call(
                "magi.changes.revert",
                serde_json::json!({"paths": ["../outside.txt"]}),
            ),
        )
        .await;
        assert!(is_error(&escaped), "{escaped}");
        let approve = rpc(
            &f,
            "tools/call",
            call("magi.changes.approve", serde_json::json!({"paths": ["a"]})),
        )
        .await;
        assert!(is_error(&approve));
    }

    #[tokio::test]
    async fn git_status_is_read_only_and_runs_without_approval() {
        let f = fixture(Profile::ReadOnly, Duration::from_secs(5));
        let git = |args: &[&str]| {
            let output = magi_process::std_command("git")
                .arg("-C")
                .arg(&f.root)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Magi Test"]);
        git(&["config", "user.email", "magi@example.test"]);
        std::fs::write(f.root.join("README.md"), "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);
        std::fs::write(f.root.join("dirty.txt"), "x").unwrap();

        let status = rpc(
            &f,
            "tools/call",
            call("magi.git.status", serde_json::json!({})),
        )
        .await;
        assert!(!is_error(&status), "{status}");
        assert!(
            status.to_string().contains(r#"\"branch\":\"main\""#),
            "{status}"
        );
        assert!(
            status.to_string().contains(r#"\"untracked\":1"#),
            "{status}"
        );
        // 变更类 git 操作不在目录里。
        let push = rpc(
            &f,
            "tools/call",
            call("magi.git.push", serde_json::json!({})),
        )
        .await;
        assert!(is_error(&push));
    }

    // ── 威胁演练（WP15）：越权、逃逸、别名与泄露 ─────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_pointing_outside_the_workspace_cannot_be_read_or_written_through() {
        let f = fixture(Profile::Edit, Duration::from_secs(2));
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "TOP-SECRET").unwrap();
        std::os::unix::fs::symlink(outside.path(), f.root.join("link")).unwrap();

        let read = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.read",
                serde_json::json!({"path": "link/secret.txt"}),
            ),
        )
        .await;
        assert!(is_error(&read), "{read}");
        assert!(!read.to_string().contains("TOP-SECRET"));
        let write = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "link/new.txt", "content": "x"}),
            ),
        )
        .await;
        assert!(is_error(&write), "{write}");
        assert!(!outside.path().join("new.txt").exists());
    }

    #[tokio::test]
    async fn patch_paths_git_dir_and_internal_or_cased_names_cannot_bypass_the_guards() {
        let f = fixture(Profile::Edit, Duration::from_secs(2));
        let patch = "*** Begin Patch\n*** Add File: ../escaped.txt\n+x\n*** End Patch\n";
        let escaped = rpc(
            &f,
            "tools/call",
            call("magi.fs.apply_patch", serde_json::json!({"patch": patch})),
        )
        .await;
        assert!(is_error(&escaped), "{escaped}");
        assert!(!f.root.parent().unwrap().join("escaped.txt").exists());

        std::fs::create_dir_all(f.root.join(".git")).unwrap();
        let git_write = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": ".git/config", "content": "x"}),
            ),
        )
        .await;
        assert!(is_error(&git_write), "{git_write}");

        for name in [
            "file_write",
            "FILE_WRITE",
            "MAGI.FS.WRITE",
            "magi.fs.write ",
            "magi.shell.exec",
        ] {
            let response = rpc(
                &f,
                "tools/call",
                call(
                    name,
                    serde_json::json!({"path": "b.txt", "content": "x", "command": "id"}),
                ),
            )
            .await;
            assert!(is_error(&response), "{name}: {response}");
        }
        assert!(!f.root.join("b.txt").exists());
    }

    #[tokio::test]
    async fn audit_and_events_never_contain_file_content_or_the_token_secret() {
        let f = fixture(Profile::EditTrusted, Duration::from_secs(2));
        let response = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "c.txt", "content": "SUPER-PRIVATE-CONTENT"}),
            ),
        )
        .await;
        assert!(!is_error(&response), "{response}");
        let audit = serde_json::to_string(&f.state.mcp_service.audit().recent(50, None)).unwrap();
        assert!(audit.contains("magi.fs.write"));
        assert!(!audit.contains("SUPER-PRIVATE-CONTENT"));
        assert!(!audit.contains(&f.secret));
        let events = serde_json::to_string(&f.state.event_bus.snapshot()).unwrap();
        assert!(!events.contains(&f.secret), "事件流不得出现令牌原文");
    }

    #[tokio::test]
    async fn trusted_profile_writes_without_approval_but_remove_still_asks() {
        let f = fixture(Profile::EditTrusted, Duration::from_millis(300));
        std::fs::write(f.root.join("gone.txt"), "x").unwrap();
        let written = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "d.txt", "content": "ok"}),
            ),
        )
        .await;
        assert!(!is_error(&written), "{written}");
        // 删除永远逐次确认：无人处理 → 超时 → 文件仍在。
        let removed = rpc(
            &f,
            "tools/call",
            call("magi.fs.remove", serde_json::json!({"path": "gone.txt"})),
        )
        .await;
        assert!(is_error(&removed), "{removed}");
        assert!(f.root.join("gone.txt").exists());
    }

    // ── 网关动态目录：项目允许的内置工具 / 下游 MCP / Skill handler ──────────────

    async fn tool_names(f: &Fixture) -> Vec<String> {
        rpc(f, "tools/list", serde_json::json!({})).await["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn gateway_lists_extra_builtins_and_downstream_mcp_but_never_context_bound_tools() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        let names = tool_names(&f).await;
        assert!(
            names.contains(&"mcp.mcp__docs__search".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"mcp.mcp__docs__publish".to_string()));
        // 需要 Task / 会话 / 浏览器上下文的内置工具不对外开放。
        for hidden in [
            "agent_spawn",
            "update_plan",
            "memory_write",
            "get_goal",
            "context_read",
        ] {
            assert!(!names.contains(&format!("magi.{hidden}")), "{hidden}");
        }
        assert!(
            !names.iter().any(|name| name.starts_with("magi.browser_")),
            "浏览器工具不对外开放: {names:?}"
        );
        // 静态目录里的内置工具不重复出现。
        assert!(!names.contains(&"magi.file_read".to_string()));
        assert!(
            !names.contains(&"magi.shell.exec".to_string()),
            "命令执行只对 Exec 档开放"
        );
        let read_only = fixture(Profile::ReadOnly, Duration::from_secs(5));
        let read_only_names = tool_names(&read_only).await;
        assert!(read_only_names.contains(&"mcp.mcp__docs__search".to_string()));
        assert!(!read_only_names.contains(&"mcp.mcp__docs__publish".to_string()));
        let exec = fixture(Profile::Exec, Duration::from_secs(5));
        let exec_names = tool_names(&exec).await;
        assert!(exec_names.contains(&"magi.shell.exec".to_string()));
        assert!(
            !exec_names.contains(&"magi.shell_exec".to_string()),
            "内部名不得作为公开名重复出现"
        );
    }

    #[tokio::test]
    async fn connector_tool_digest_is_stable_for_the_same_catalog() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        let first = web_slot_tool_digest(&f.state).expect("catalog digest");
        assert_eq!(Some(first.clone()), web_slot_tool_digest(&f.state));
        assert_eq!(first.len(), 64);
    }

    #[tokio::test]
    async fn web_slot_tool_count_matches_the_tools_list_catalog() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        let listed_count = tool_names(&f).await.len();

        assert_eq!(
            web_slot_tool_count(&f.state),
            u64::try_from(listed_count).ok(),
            "connector status count must come from the same tools/list catalog"
        );
    }

    #[tokio::test]
    async fn read_only_downstream_tool_runs_without_approval_and_write_tool_always_asks() {
        let f = fixture(Profile::EditTrusted, Duration::from_millis(400));
        let search = rpc(
            &f,
            "tools/call",
            call("mcp.mcp__docs__search", serde_json::json!({})),
        )
        .await;
        assert!(!is_error(&search), "{search}");
        assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // 即使是“免确认编辑”档，副作用未知的下游工具也要逐次确认：无人处理则超时且不执行。
        let publish = rpc(
            &f,
            "tools/call",
            call("mcp.mcp__docs__publish", serde_json::json!({})),
        )
        .await;
        assert!(is_error(&publish), "{publish}");
        assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let f = fixture(Profile::Edit, Duration::from_secs(10));
        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let approved = rpc(
            &f,
            "tools/call",
            call("mcp.mcp__docs__publish", serde_json::json!({})),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&approved), "{approved}");
        assert!(approved.to_string().contains("publish"));
        assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn extra_builtin_runs_through_the_same_policy_and_unknown_dynamic_names_are_refused() {
        let f = fixture(Profile::Edit, Duration::from_secs(5));
        for name in [
            "mcp.nope",
            "skill.nope",
            "magi.agent_spawn",
            "mcp.mcp__docs__missing",
        ] {
            let reply = rpc(&f, "tools/call", call(name, serde_json::json!({}))).await;
            assert!(is_error(&reply), "{name}: {reply}");
        }
    }
}
