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
    ApprovalMode, AttributionTarget, AuditEvent, AuditOutcome, AuditSink, DynamicTool,
    InvocationOutcome, PathAccess, PathRequest, Profile, TokenStore, ToolBackend, ToolClass,
    ToolInvocation, ToolSchema, ToolSchemaProvider, WorkspaceResolver,
};
use magi_tool_runtime::{BuiltinToolName, tool_path_access_requests};
use serde_json::Value;

use crate::state::ApiState;

/// 等待 Magi 界面人工确认的默认时限。超时后调用以工具错误收口，不会稍后补执行。
pub(crate) const EXTERNAL_APPROVAL_TIMEOUT: Duration = Duration::from_secs(90);

/// 每次调用唯一的 id：同一毫秒内的并发调用也不会撞号；审批、活动与槽位写回共用它。
fn next_call_id(token_id: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "mcp-{token_id}-{}-{}",
        UtcMillis::now().0,
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// 外部工具会话 id 的前缀；前端据此识别外部会话。
pub(crate) const EXTERNAL_SESSION_ID_PREFIX: &str = "external-tool-";

/// 外部令牌对应的外部工具会话 id。
pub(crate) fn external_session_id(token_id: &str) -> SessionId {
    SessionId::new(format!("{EXTERNAL_SESSION_ID_PREFIX}{token_id}"))
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

pub(crate) fn workspace_name(state: &ApiState, workspace_id: &str) -> Option<String> {
    state
        .workspace_registry
        .workspaces()
        .into_iter()
        .find(|workspace| workspace.workspace_id.as_str() == workspace_id)
        .map(|workspace| {
            workspace
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| {
                    Path::new(workspace.root_path.as_str())
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| workspace.root_path.to_string())
                })
        })
}

impl WorkspaceResolver for RegistryWorkspaces {
    fn root_of(&self, workspace_id: &str) -> Option<PathBuf> {
        self.state
            .workspace_root_path(&Some(WorkspaceId::new(workspace_id)))
    }

    fn root_for(
        &self,
        principal: &magi_mcp_server::ClientPrincipal,
        target: &AttributionTarget,
    ) -> Option<PathBuf> {
        match target {
            AttributionTarget::ExternalSession { .. } => self.root_of(&principal.workspace_id),
            AttributionTarget::WebSlotTurn {
                session_id,
                turn_id,
            } => {
                let (owner, active) = self.state.web_model.bindings()?.active_context()?;
                if owner.session_id != *session_id
                    || active.to_string() != *turn_id
                    || owner.project_id != principal.workspace_id
                {
                    return None;
                }
                let id = SessionId::new(session_id);
                let session = self.state.session_store.session(&id)?;
                if session.workspace_id.as_deref().unwrap_or("") != principal.workspace_id {
                    return None;
                }
                match self.state.session_workspace_id(&session) {
                    Some(workspace) => self
                        .state
                        .workspace_root_path(&Some(workspace))
                        .map(|root| self.state.snapshot_root_for_session(&id, &root)),
                    None => self.state.personal_session_execution_root(&id).ok(),
                }
            }
        }
    }

    fn name_of(&self, workspace_id: &str) -> Option<String> {
        workspace_name(&self.state, workspace_id)
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
        // 在进入工具前就被拒绝的外部调用也要在外部会话里看得到（GPT Web 槽位的调用归它自己的会话）。
        if let AuditOutcome::Denied { reason } = &event.outcome
            && event.token_id != crate::web_slot_mcp::WEB_SLOT_PRINCIPAL_ID
        {
            record_denied_external_call(&self.state, &event, reason);
        }
        let workspace_id =
            (!event.workspace_id.is_empty()).then(|| WorkspaceId::new(event.workspace_id.clone()));
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
                workspace_id,
                ..EventContext::default()
            }),
        );
    }
}

/// 内置工具在网关里的风险类别：供 GPT Web 槽位按工具档决策；MCP 令牌不按类别过滤，由授权方式统一处理。
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

    /// 变更账本工具：直接操作该外部会话的 `SnapshotSession`。
    async fn run_changes_tool(
        &self,
        invocation: &ToolInvocation,
        call_id: &str,
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
                if invocation.requires_approval
                    && let Err(message) = self
                        .request_approval(invocation, call_id, session_id, workspace_id, root)
                        .await
                {
                    return error_outcome(message);
                }
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
                if invocation.requires_approval
                    && let Err(message) = self
                        .request_approval(invocation, call_id, session_id, workspace_id, root)
                        .await
                {
                    return error_outcome(message);
                }
                // 审批等待期间文件可能又被改过，落盘前再确认一次。
                match check_revert_conflicts(snapshot.clone(), paths.clone()).await {
                    Ok(None) => {}
                    Ok(Some(message)) => return error_outcome(message),
                    Err(message) => return error_outcome(message),
                }
                let cancellation = invocation.cancellation.clone();
                let reverted = tokio::task::spawn_blocking(move || {
                    if cancellation.is_cancelled() {
                        return Ok(None);
                    }
                    snapshot.revert(&paths).map(Some)
                })
                .await;
                match reverted {
                    Ok(Ok(Some(count))) => InvocationOutcome {
                        text: serde_json::json!({ "reverted": count }).to_string(),
                        is_error: false,
                    },
                    Ok(Ok(None)) => cancelled_outcome(),
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
            AttributionTarget::ExternalSession { token_id } => ensure_external_session(
                &self.state,
                token_id,
                &invocation.principal.client_name,
                &invocation.principal.workspace_id,
            ),
            AttributionTarget::WebSlotTurn { session_id, .. } => {
                if !(RegistryWorkspaces {
                    state: self.state.clone(),
                })
                .root_for(&invocation.principal, &invocation.attribution)
                .is_some()
                {
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

    async fn request_approval(
        &self,
        invocation: &ToolInvocation,
        call_id: &str,
        session_id: &SessionId,
        workspace_id: &WorkspaceId,
        root: &Path,
    ) -> Result<(), String> {
        let state = self.state.clone();
        let tokens = self.tokens.clone();
        let timeout = self.approval_timeout;
        let invocation = invocation.clone();
        let call_id = call_id.to_string();
        let session_id = session_id.clone();
        let workspace_id = workspace_id.clone();
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            approve_blocking(
                &state,
                &tokens,
                timeout,
                &invocation,
                &call_id,
                &session_id,
                Some(&workspace_id),
                &root,
            )
        })
        .await
        .map_err(|_| "审批等待意外中断".to_string())?
    }
}

/// 外部令牌的外部会话；不存在时创建（并发创建时另一方已建好属于正常竞态）。
pub(crate) fn ensure_external_session(
    state: &ApiState,
    token_id: &str,
    client_name: &str,
    workspace_id: &str,
) -> Result<SessionId, String> {
    let session_id = external_session_id(token_id);
    if state.session_store.session(&session_id).is_none() {
        let title = format!("{client_name} · 外部工具");
        if let Err(error) = state.session_store.create_external_tool_session(
            session_id.clone(),
            title,
            workspace_id.to_string(),
        ) && state.session_store.session(&session_id).is_none()
        {
            return Err(format!("创建外部工具会话失败: {error}"));
        }
    }
    Ok(session_id)
}

/// 进入工具之前就被拒绝的外部调用（权限档、路径范围、参数上限）：作为一条已拒绝的工具项
/// 写进外部会话，结果文本是拒绝原因。写入失败只记日志，不影响拒绝本身。
fn record_denied_external_call(state: &ApiState, event: &AuditEvent, reason: &str) {
    let session_id = match ensure_external_session(
        state,
        &event.token_id,
        &event.client_name,
        &event.workspace_id,
    ) {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, "无法为被拒绝的外部调用建立外部会话");
            return;
        }
    };
    let workspace_id = Some(WorkspaceId::new(event.workspace_id.clone()));
    let turns = &state.mcp_service.external_turns;
    let turn_id = match turns.begin_call(
        state,
        &session_id,
        &WorkspaceId::new(event.workspace_id.clone()),
        &event.client_name,
    ) {
        Ok(turn_id) => turn_id,
        Err(error) => {
            tracing::warn!(%error, "被拒绝的外部调用无法写入外部会话");
            return;
        }
    };
    let writer = magi_conversation_runtime::session_writeback::ExternalToolItemWriter {
        session_store: &state.session_store,
        event_bus: &state.event_bus,
        session_id: &session_id,
        workspace_id: &workspace_id,
        turn_id: Some(&turn_id),
        origin: None,
    };
    let arguments = serde_json::json!({ "paths": event.paths }).to_string();
    // 与其他策略拒绝同一种结构，工具卡片按拒绝展示，而不是“执行异常”。
    let rejection = serde_json::json!({
        "tool": &event.tool,
        "status": "rejected",
        "error_code": "external_tool_policy_rejected",
        "error": reason,
    })
    .to_string();
    let internal_name = magi_mcp_server::catalog::mapping_for_public_name(&event.tool)
        .map(|mapping| mapping.internal_name.to_string())
        .unwrap_or_else(|| event.tool.clone());
    if let Err(error) = writer.finished(
        &next_call_id(&event.token_id),
        &internal_name,
        &arguments,
        &rejection,
        ExecutionResultStatus::Rejected,
    ) {
        tracing::warn!(%error, "被拒绝的外部调用写入失败");
    }
    turns.end_call(state, &session_id);
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

/// 调用存活判据：外部令牌要求同一密钥仍有效；槽位调用要求它归属的 Web turn 仍在进行
/// （槽位释放或 turn 结束都会使它失效）。
fn liveness(
    state: &ApiState,
    tokens: &Arc<TokenStore>,
    invocation: &ToolInvocation,
) -> impl Fn() -> bool + Send + 'static {
    let tokens = tokens.clone();
    let state = state.clone();
    let token_id = invocation.principal.token_id.clone();
    let token_hash = tokens
        .records()
        .iter()
        .find(|record| {
            record.token_id == token_id && record.prefix == invocation.principal.token_prefix
        })
        .map(|record| record.hash.clone());
    let attribution = invocation.attribution.clone();
    let cancellation = invocation.cancellation.clone();
    move || {
        !cancellation.is_cancelled()
            && match &attribution {
                AttributionTarget::WebSlotTurn {
                    session_id,
                    turn_id,
                } => state
                    .web_model
                    .bindings()
                    .and_then(|slots| slots.active_context())
                    .is_some_and(|(owner, active)| {
                        owner.session_id == *session_id && active.to_string() == *turn_id
                    }),
                AttributionTarget::ExternalSession { .. } => {
                    tokens.records().iter().any(|record| {
                        record.token_id == token_id
                            && record.is_active(UtcMillis::now().0)
                            && Some(&record.hash) == token_hash.as_ref()
                    })
                }
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

fn cancelled_outcome() -> InvocationOutcome {
    error_outcome(serde_json::json!({ "status": "cancelled", "error_code": "tool_cancelled", "error": "调用已取消，工具未执行" }).to_string())
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

    fn tool_available(&self, principal: &magi_mcp_server::ClientPrincipal, name: &str) -> bool {
        if principal.attribution != magi_mcp_server::AttributionMode::FollowWebSlot {
            return true;
        }
        let Some(snapshot) = self
            .state
            .web_model
            .bindings()
            .and_then(|slots| slots.snapshot())
        else {
            return true;
        };
        if snapshot.owner_project_id.is_empty() {
            return !name.starts_with("changes_")
                && !BuiltinToolName::from_name(name)
                    .is_some_and(|tool| tool.requires_workspace_context());
        }
        true
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
            let call_id = next_call_id(&invocation.principal.token_id);
            // 记录开始项前也检查真实会话与槽位代际，不能先向旧归属写入。
            if let Err(error) = self.resolve_session(&invocation) {
                return error_outcome(error);
            }
            let workspace_id = (!invocation.principal.workspace_id.is_empty())
                .then(|| WorkspaceId::new(invocation.principal.workspace_id.clone()));
            let turns = &self.state.mcp_service.external_turns;
            // 调用都作为普通工具项写进 canonical（§8.4）：GPT Web 槽位写进拥有者会话当前的 Turn，
            // 外部令牌写进它自己外部会话的活动段 Turn。
            let target = match &invocation.attribution {
                AttributionTarget::WebSlotTurn { session_id, .. } => Some((
                    SessionId::new(session_id.clone()),
                    None,
                    Some(magi_conversation_runtime::session_writeback::TOOL_ORIGIN_CHATGPT_WEB),
                )),
                AttributionTarget::ExternalSession { .. } => {
                    let session_id = match self.resolve_session(&invocation) {
                        Ok(id) => id,
                        Err(error) => return error_outcome(error),
                    };
                    match turns.begin_call(
                        &self.state,
                        &session_id,
                        &WorkspaceId::new(invocation.principal.workspace_id.clone()),
                        &invocation.principal.client_name,
                    ) {
                        Ok(turn_id) => Some((session_id, Some(turn_id), None)),
                        Err(error) => {
                            // 记录失败不应让外部客户端的调用整体不可用：照常执行，只是这次没有展示。
                            tracing::warn!(%error, "外部调用无法写入外部会话时间线");
                            None
                        }
                    }
                }
            };
            let external_session_id = matches!(
                invocation.attribution,
                AttributionTarget::ExternalSession { .. }
            )
            .then(|| target.as_ref().map(|(session_id, _, _)| session_id.clone()))
            .flatten();
            let save_content = self.state.mcp_service.save_content();
            let recorded_arguments = if save_content {
                invocation.arguments.to_string()
            } else {
                redact_arguments(&invocation.arguments)
            };
            let tool_name = invocation.internal_name.clone();
            let writer = target.as_ref().map(|(session_id, turn_id, origin)| {
                magi_conversation_runtime::session_writeback::ExternalToolItemWriter {
                    session_store: &self.state.session_store,
                    event_bus: &self.state.event_bus,
                    session_id,
                    workspace_id: &workspace_id,
                    turn_id: turn_id.as_deref(),
                    origin: *origin,
                }
            });
            if let Some(writer) = &writer
                && let Err(error) = writer.started(&call_id, &tool_name, &recorded_arguments)
            {
                if external_session_id.is_none() {
                    return error_outcome(format!("无法记录到当前 Web 对话：{error}"));
                }
                tracing::warn!(%error, "外部调用开始项写入失败");
            }
            // 身份失效也须停止在途调用，而不只是拒绝下一条请求。复用工具运行时取消
            // 标记，并等待执行层结算；不可中断的操作仍记录其真实结果。
            let is_alive = liveness(&self.state, &self.tokens, &invocation);
            let cancellation = invocation.cancellation.flag();
            if !is_alive() {
                cancellation.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let execution = self.invoke_resolved(invocation, &call_id);
            tokio::pin!(execution);
            let outcome = loop {
                tokio::select! {
                    outcome = &mut execution => break outcome,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if !is_alive() {
                            cancellation.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }
            };
            if let Some(writer) = &writer {
                let status = if outcome.is_error
                    && magi_conversation_runtime::tool_result_utils::tool_result_execution_status(
                        &outcome.text,
                    ) == ExecutionResultStatus::Cancelled
                {
                    ExecutionResultStatus::Cancelled
                } else if outcome.is_error {
                    ExecutionResultStatus::Failed
                } else {
                    ExecutionResultStatus::Succeeded
                };
                let recorded_result = if save_content {
                    outcome.text.clone()
                } else {
                    CONTENT_NOT_SAVED.to_string()
                };
                if let Err(error) = writer.finished(
                    &call_id,
                    &tool_name,
                    &recorded_arguments,
                    &recorded_result,
                    status,
                ) {
                    tracing::warn!(%error, "工具结果写入 canonical 失败");
                }
            }
            if let Some(session_id) = &external_session_id {
                turns.end_call(&self.state, session_id);
            }
            outcome
        })
    }
}

/// 关闭“保存调用内容”后，结果文本在外部会话里的占位。
const CONTENT_NOT_SAVED: &str = "（内容未保存：已在设置里关闭「保存调用内容」）";

/// 超过这个长度的字符串参数（文件内容、补丁、命令输出等）在不保存内容时被省略。
const REDACT_STRING_BYTES: usize = 200;

/// 不保存调用内容时的参数：保留路径、查询、命令这类描述动作的短字段，省略大段内容。
fn redact_arguments(arguments: &Value) -> String {
    fn redact(value: &Value) -> Value {
        match value {
            Value::String(text) if text.len() > REDACT_STRING_BYTES => {
                Value::String("（内容已省略）".to_string())
            }
            Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), redact(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    redact(arguments).to_string()
}

impl ApiToolBackend {
    async fn invoke_resolved(
        &self,
        invocation: ToolInvocation,
        call_id: &str,
    ) -> InvocationOutcome {
        if invocation.cancellation.is_cancelled() {
            return cancelled_outcome();
        }
        {
            let Some(registry) = self.state.tool_registry().cloned() else {
                return error_outcome("工具运行时未就绪");
            };
            let session_id = match self.resolve_session(&invocation) {
                Ok(id) => id,
                Err(error) => return error_outcome(error),
            };
            let workspace_id = (!invocation.principal.workspace_id.is_empty())
                .then(|| WorkspaceId::new(invocation.principal.workspace_id.clone()));
            let Some(root) = (RegistryWorkspaces {
                state: self.state.clone(),
            })
            .root_for(&invocation.principal, &invocation.attribution) else {
                return error_outcome("令牌绑定的工作区不可用");
            };
            let class = Some(invocation.class);
            if invocation.internal_name.starts_with("changes_") {
                let Some(workspace_id) = workspace_id.as_ref() else {
                    return error_outcome("个人会话没有项目变更账本");
                };
                return self
                    .run_changes_tool(&invocation, call_id, &session_id, workspace_id, &root)
                    .await;
            }
            // git 工具需要会话的 Git context；由变更面板同一入口建立（幂等）。
            if invocation.internal_name.starts_with("git_")
                && let Some(workspace_id) = workspace_id.as_ref()
                && let Err(error) = self
                    .state
                    .synchronize_session_changes(&session_id, workspace_id, &root, false)
                    .await
            {
                return error_outcome(format!("无法建立 Git 上下文: {}", error.message()));
            }
            // 写入类调用才需要会话账本；只读调用不触发整树快照的懒启动。
            let snapshot = if workspace_id.is_none() || matches!(class, Some(ToolClass::Read)) {
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
            let call_id = call_id.to_string();
            let joined = tokio::task::spawn_blocking(move || {
                run_blocking(
                    &state,
                    &tokens,
                    approval_timeout,
                    &registry,
                    &invocation,
                    &call_id,
                    &session_id,
                    workspace_id.as_ref(),
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

/// GPT Web 槽位按设置里的工具档和授权方式处理；外部令牌按令牌的授权方式处理。
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
        match invocation.principal.approval_mode {
            ApprovalMode::Ask => crate::web_model_channel::WebApprovalMode::Ask,
            ApprovalMode::Always => crate::web_model_channel::WebApprovalMode::Always,
        }
    }
}

/// GPT Web 的「始终授权」保留高风险操作的逐次确认边界；MCP 令牌的开放权限由上游直接处理。
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
#[allow(clippy::too_many_arguments)]
fn approve_blocking(
    state: &ApiState,
    tokens: &Arc<TokenStore>,
    timeout: Duration,
    invocation: &ToolInvocation,
    call_id: &str,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    root: &Path,
) -> Result<(), String> {
    match slot_approval_mode(state, invocation) {
        crate::web_model_channel::WebApprovalMode::Always => return Ok(()),
        crate::web_model_channel::WebApprovalMode::Deny => {
            return Err(WEB_APPROVAL_DENIED_BY_SETTING.to_string());
        }
        crate::web_model_channel::WebApprovalMode::Ask => {}
    }
    let is_alive = liveness(state, tokens, invocation);
    let approval = await_external_tool_approval(
        &state.event_bus,
        state.conversation_registry.tool_approvals(),
        &ExternalApprovalRequest {
            session_id,
            workspace_id,
            token_id: &invocation.principal.token_id,
            client_name: &invocation.principal.client_name,
            token_prefix: &invocation.principal.token_prefix,
            tool_call_id: call_id,
            tool_name: &invocation.internal_name,
            arguments_json: &invocation.arguments.to_string(),
            summary: &approval_summary(invocation, root),
            timeout,
        },
        &is_alive,
    );
    approval.map_err(|(payload, _)| payload)
}

#[allow(clippy::too_many_arguments)]
fn run_blocking(
    state: &ApiState,
    tokens: &Arc<TokenStore>,
    approval_timeout: Duration,
    registry: &magi_tool_runtime::ToolRegistry,
    invocation: &ToolInvocation,
    call_id: &str,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    root: &Path,
    snapshot: Option<&dyn magi_snapshot::ToolHook>,
) -> InvocationOutcome {
    let arguments_json = invocation.arguments.to_string();
    let safety_gate =
        magi_conversation_runtime::safety_gate_from_settings(Some(&state.settings_store));
    let is_alive = liveness(state, tokens, invocation);
    let summary = approval_summary(invocation, root);
    let approve = |approval_id: &str| {
        await_external_tool_approval(
            &state.event_bus,
            state.conversation_registry.tool_approvals(),
            &ExternalApprovalRequest {
                session_id,
                workspace_id,
                token_id: &invocation.principal.token_id,
                client_name: &invocation.principal.client_name,
                token_prefix: &invocation.principal.token_prefix,
                tool_call_id: approval_id,
                tool_name: &invocation.internal_name,
                arguments_json: &arguments_json,
                summary: &summary,
                timeout: approval_timeout,
            },
            &is_alive,
        )
    };

    let approval_mode = slot_approval_mode(state, invocation);
    if invocation.cancellation.is_cancelled() {
        return cancelled_outcome();
    }
    // MCP 的开放权限代表用户已授予完整工具调用；传入 granted 让既有执行链跳过重复审批，
    // 但路径越界等硬性策略仍由执行层拒绝。
    let mut granted = approval_mode == crate::web_model_channel::WebApprovalMode::Always;
    if invocation.requires_approval {
        match approval_mode {
            crate::web_model_channel::WebApprovalMode::Always => {}
            crate::web_model_channel::WebApprovalMode::Ask => {
                if let Err((payload, _)) = approve(call_id) {
                    return error_outcome(payload);
                }
            }
            crate::web_model_channel::WebApprovalMode::Deny => {
                return error_outcome(WEB_APPROVAL_DENIED_BY_SETTING);
            }
        }
        granted = true;
    }
    let access_profile = access_profile_for(invocation.principal.profile);
    let external_call = ExternalToolCall {
        cancellation: Some(invocation.cancellation.flag()),
        call_id,
        tool_name: &invocation.internal_name,
        arguments_json: &arguments_json,
        session_id,
        workspace_id,
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

/// 执行层访问档映射：只读 GPT Web 槽位收紧为 ReadOnly；其余使用 Restricted。
/// MCP 的 `always` 会在既有执行链中请求授权后的 FullAccess，但仍受路径和硬性策略限制。
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
    if !state.web_model_plugin_active() {
        return None;
    }
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
    Some(server.catalog_definition_for(&principal))
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
    use magi_mcp_server::{ApprovalMode, AttributionMode, IssueTokenRequest};
    use magi_session_store::{SessionKind, SessionStore};
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use tower::ServiceExt;

    struct Fixture {
        state: ApiState,
        router: axum::Router,
        transport_session: tokio::sync::OnceCell<String>,
        #[cfg(unix)]
        tokens: Arc<TokenStore>,
        secret: String,
        token_id: String,
        root: PathBuf,
        mcp_calls: Arc<std::sync::atomic::AtomicUsize>,
        _dir: tempfile::TempDir,
    }

    fn fixture(approval_timeout: Duration) -> Fixture {
        fixture_with_approval(ApprovalMode::Ask, approval_timeout)
    }

    fn fixture_with_approval(approval_mode: ApprovalMode, approval_timeout: Duration) -> Fixture {
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
                task_store: None,
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
        let package =
            crate::bundled_plugins::bundled_plugin_package(magi_web_model::WEB_MODEL_PLUGIN_ID)
                .unwrap();
        let permissions = package.manifest().permissions.clone();
        {
            let mut manager = state.plugin_manager.lock().unwrap();
            manager
                .install(
                    &package,
                    magi_plugin_system::PluginSource::Builtin {
                        name: magi_web_model::WEB_MODEL_PLUGIN_ID.into(),
                    },
                )
                .unwrap();
            manager
                .authorize(
                    magi_web_model::WEB_MODEL_PLUGIN_ID,
                    "application",
                    permissions,
                )
                .unwrap();
            manager
                .enable(magi_web_model::WEB_MODEL_PLUGIN_ID, "application")
                .unwrap();
            manager
                .activate(magi_web_model::WEB_MODEL_PLUGIN_ID, "application")
                .unwrap();
        }
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
                    approval_mode,
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
            transport_session: tokio::sync::OnceCell::new(),
            router: router(HttpState::new(
                server,
                tokens.clone(),
                HttpConfig::default(),
            )),
            #[cfg(unix)]
            tokens,
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
        rpc_message(fixture, body).await
    }

    async fn http_message(
        fixture: &Fixture,
        body: Value,
        session: Option<&str>,
    ) -> axum::response::Response {
        let mut request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {}", fixture.secret));
        if let Some(id) = session {
            request = request.header("mcp-session-id", id);
        }
        fixture
            .router
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
    }

    async fn rpc_message(fixture: &Fixture, body: Value) -> Value {
        let session = fixture
            .transport_session
            .get_or_init(|| async {
                let response = http_message(
                    fixture,
                    serde_json::json!({"jsonrpc":"2.0", "id":0, "method":"initialize"}),
                    None,
                )
                .await;
                response.headers()["mcp-session-id"]
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .await;
        let response = http_message(fixture, body, Some(session)).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        }
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
    async fn client_cancellation_and_disconnect_settle_approval_without_writing() {
        for disconnect in [false, true] {
            let f = Arc::new(fixture(Duration::from_secs(10)));
            let running = {
                let f = f.clone();
                tokio::spawn(async move {
                    rpc(
                        &f,
                        "tools/call",
                        call(
                            "magi.fs.write",
                            serde_json::json!({"path":"cancelled.txt", "content":"must not write"}),
                        ),
                    )
                    .await
                })
            };
            let session = external_session_id(&f.token_id);
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if !f
                        .state
                        .conversation_registry
                        .tool_approvals()
                        .pending_for_session(&session)
                        .is_empty()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if disconnect {
                running.abort();
                assert!(running.await.unwrap_err().is_cancelled());
            } else {
                assert!(rpc_message(&f, serde_json::json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":1}})).await.is_null());
                let result = tokio::time::timeout(Duration::from_secs(5), running)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(result.to_string().contains("cancelled"), "{result}");
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if f.state.mcp_service.external_turns.in_flight(&session) == 0
                        && f.state
                            .event_bus
                            .snapshot()
                            .recent_events
                            .iter()
                            .any(|event| event.event_type == "mcp.tool.call")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                f.state
                    .conversation_registry
                    .tool_approvals()
                    .pending_for_session(&session)
                    .is_empty()
            );
            assert!(!f.root.join("cancelled.txt").exists());
            assert!(
                f.state
                    .event_bus
                    .snapshot()
                    .recent_events
                    .iter()
                    .any(|event| {
                        event.event_type == "mcp.activity"
                            && event.session_id.as_ref() == Some(&session)
                    }),
                "调用结算通知必须能通过 Desktop 的会话事件过滤"
            );
            assert!(
                external_tool_items(&f)
                    .iter()
                    .any(|(_, _, result, status)| result.contains("cancelled")
                        && status == "Cancelled")
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_stops_only_the_requested_shell_call_and_records_actual_status() {
        let f = Arc::new(fixture_with_approval(
            ApprovalMode::Always,
            Duration::from_secs(5),
        ));
        let running = {
            let f = f.clone();
            tokio::spawn(async move {
                rpc(&f, "tools/call", call("magi.shell.exec", serde_json::json!({"command":"printf started > started.txt; sleep 30; printf finished > finished.txt"}))).await
            })
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            while !f.root.join("started.txt").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let read = rpc_message(&f, serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":call("magi.fs.read", serde_json::json!({"path":"started.txt"}))})).await;
        assert!(!is_error(&read), "{read}");
        rpc_message(&f, serde_json::json!({"jsonrpc":"2.0", "method":"notifications/cancelled", "params":{"requestId":1}})).await;
        let result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap();
        assert!(result.to_string().contains("cancelled"), "{result}");
        assert!(!f.root.join("finished.txt").exists());
        assert_eq!(
            f.state
                .mcp_service
                .external_turns
                .in_flight(&external_session_id(&f.token_id)),
            0
        );
        let items = external_tool_items(&f);
        assert!(
            items
                .iter()
                .any(|(name, _, _, status)| name == "shell_exec" && status == "Cancelled"),
            "{items:?}"
        );
        assert!(
            items
                .iter()
                .any(|(name, _, _, status)| name == "file_read" && status == "Completed"),
            "{items:?}"
        );
    }

    #[tokio::test]
    async fn tokens_list_the_full_tool_catalog_including_shell() {
        let f = fixture(Duration::from_secs(5));
        let response = rpc(&f, "tools/list", serde_json::json!({})).await;
        let names = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"magi.fs.read".to_string()));
        assert!(names.contains(&"magi.fs.write".to_string()));
        assert!(
            names.contains(&"magi.shell.exec".to_string()),
            "令牌不按工具档过滤，命令执行也在目录里（是否询问由授权方式决定）"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalidating_a_token_stops_its_running_shell_and_records_cancellation() {
        for invalidation in ["revoke", "expire", "rotate"] {
            let f = Arc::new(fixture_with_approval(
                ApprovalMode::Always,
                Duration::from_secs(5),
            ));
            let running = {
                let f = f.clone();
                tokio::spawn(async move {
                    rpc(&f, "tools/call", call("magi.shell.exec", serde_json::json!({"command":"printf started > started.txt; sleep 2; printf finished > finished.txt"}))).await
                })
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                while !f.root.join("started.txt").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            match invalidation {
                "revoke" => assert!(f.tokens.revoke(&f.token_id, UtcMillis::now().0)),
                "expire" => {
                    f.tokens
                        .update(
                            &f.token_id,
                            magi_mcp_server::TokenPatch {
                                expires_at_ms: Some(Some(UtcMillis::now().0)),
                                ..Default::default()
                            },
                            UtcMillis::now().0,
                        )
                        .unwrap();
                }
                "rotate" => {
                    f.tokens.rotate(&f.token_id).unwrap();
                }
                _ => unreachable!(),
            }
            let result = tokio::time::timeout(Duration::from_secs(5), running)
                .await
                .unwrap()
                .unwrap();
            assert!(
                is_error(&result) && result.to_string().contains("cancelled"),
                "{result}"
            );
            assert!(!f.root.join("finished.txt").exists());
            assert_eq!(
                f.state
                    .mcp_service
                    .external_turns
                    .in_flight(&external_session_id(&f.token_id)),
                0
            );
            assert!(
                external_tool_items(&f)
                    .iter()
                    .any(|(name, _, _, status)| name == "shell_exec" && status == "Cancelled")
            );
        }
    }

    #[tokio::test]
    async fn open_mode_reads_without_prompt_and_paths_outside_the_workspace_are_denied() {
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
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

    /// 外部会话在 canonical 里的工具项：(工具名, 参数文本, 结果文本, 状态)。
    fn external_tool_items(f: &Fixture) -> Vec<(String, String, String, String)> {
        f.state
            .session_store
            .canonical_turns_for_session(&external_session_id(&f.token_id))
            .into_iter()
            .flat_map(|turn| turn.items)
            .filter_map(|item| {
                let tool = item.tool?;
                Some((
                    tool.name,
                    tool.arguments
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    tool.result
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    format!("{:?}", item.status),
                ))
            })
            .collect()
    }

    #[tokio::test]
    async fn external_calls_land_in_the_external_session_timeline_with_results_and_denials() {
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
        std::fs::write(f.root.join("hello.txt"), "hi there").unwrap();
        let ok = rpc(
            &f,
            "tools/call",
            call("magi.fs.read", serde_json::json!({"path": "hello.txt"})),
        )
        .await;
        assert!(!is_error(&ok), "{ok}");
        let denied = rpc(
            &f,
            "tools/call",
            call("magi.fs.read", serde_json::json!({"path": "../escape.txt"})),
        )
        .await;
        assert!(is_error(&denied), "{denied}");

        let session_id = external_session_id(&f.token_id);
        let turns = f
            .state
            .session_store
            .canonical_turns_for_session(&session_id);
        assert_eq!(turns.len(), 1, "连续的外部调用在同一个活动段 Turn 里");
        let items = external_tool_items(&f);
        let read = items
            .iter()
            .find(|item| item.2.contains("hi there"))
            .expect("成功的读取带着读到的内容");
        assert!(read.1.contains("hello.txt"));
        let refused = items
            .iter()
            .find(|item| item.1.contains("paths"))
            .expect("被拒绝的越界读取也是一条工具项");
        assert!(!refused.2.is_empty(), "结果文本是拒绝原因");
        assert_eq!(f.state.mcp_service.external_turns.in_flight(&session_id), 0);
    }

    #[tokio::test]
    async fn turning_off_content_saving_keeps_actions_but_not_results() {
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
        f.state
            .mcp_service
            .set_retention(crate::mcp_runtime::ExternalRetention {
                save_content: false,
                retention_days: 30,
            })
            .await
            .unwrap();
        std::fs::write(f.root.join("secret.txt"), "top secret body").unwrap();
        let ok = rpc(
            &f,
            "tools/call",
            call("magi.fs.read", serde_json::json!({"path": "secret.txt"})),
        )
        .await;
        assert!(!is_error(&ok), "{ok}");
        let items = external_tool_items(&f);
        assert!(
            items.iter().any(|item| item.1.contains("secret.txt")),
            "动作与路径保留"
        );
        assert!(
            items.iter().all(|item| !item.2.contains("top secret body")),
            "关闭保存内容后读到的内容不落库: {items:?}"
        );
        assert!(items.iter().any(|item| item.2.contains("内容未保存")));
    }

    #[tokio::test]
    async fn approved_write_lands_in_the_external_session_ledger() {
        let f = fixture(Duration::from_secs(10));
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
    async fn ask_mode_prompts_for_each_write_in_one_activity() {
        let f = fixture(Duration::from_millis(400));
        let approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let first = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "activity-first.txt", "content": "first"}),
            ),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&first), "{first}");

        let second = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "activity-second.txt", "content": "second"}),
            ),
        )
        .await;
        assert!(
            is_error(&second),
            "同一活动段内的第二次写入仍需授权: {second}"
        );
        assert!(second.to_string().contains("tool_approval_timeout"));
        assert!(!f.root.join("activity-second.txt").exists());
        assert_eq!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&external_session_id(&f.token_id))
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn denied_or_timed_out_write_leaves_the_file_untouched() {
        let f = fixture(Duration::from_millis(400));
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
    async fn changes_list_shows_own_writes_and_revert_restores_after_approval() {
        let f = fixture(Duration::from_secs(10));
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

        let list_approver = resolve_pending(
            &f.state,
            &f.token_id,
            magi_conversation_runtime::ToolApprovalDecision::AllowOnce,
        );
        let listed = rpc(
            &f,
            "tools/call",
            call("magi.changes.list", serde_json::json!({})),
        )
        .await;
        list_approver.join().unwrap();
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
        let f = fixture(Duration::from_secs(10));
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
        let f = fixture(Duration::from_secs(5));
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
    async fn open_mode_git_status_runs_without_approval() {
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(5));
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
        let f = fixture(Duration::from_secs(2));
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
        let f = fixture(Duration::from_secs(2));
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
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(2));
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
    async fn open_mode_still_rejects_hard_blocked_commands_without_prompting() {
        // 开放权限只免掉人工确认；用户配置的 HardBlock 规则、路径与访问档限制仍然拒绝。
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_secs(2));
        f.state
            .settings_store
            .set_section(
                "safeguardConfig",
                serde_json::json!({"rules": [{
                    "pattern": "magi-hard-block-probe",
                    "enabled": true,
                    "category": "custom",
                    "action": "hard_block"
                }]}),
            )
            .unwrap();
        let blocked = rpc(
            &f,
            "tools/call",
            call(
                "magi.shell.exec",
                serde_json::json!({"command": "touch hard-block-ran.txt && echo magi-hard-block-probe"}),
            ),
        )
        .await;
        assert!(
            is_error(&blocked),
            "开放模式也必须拒绝 HardBlock: {blocked}"
        );
        assert!(
            blocked.to_string().contains("tool_safety_rejected"),
            "拒绝原因必须是安全防护硬拦截: {blocked}"
        );
        assert!(
            !f.root.join("hard-block-ran.txt").exists(),
            "被硬拦截的命令不得产生任何副作用"
        );
        assert!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&external_session_id(&f.token_id))
                .is_empty(),
            "硬拦截是直接拒绝，不是转成待确认"
        );

        // 同一令牌的其它命令不受影响：只拦命中规则的那一条。
        let ok = rpc(
            &f,
            "tools/call",
            call(
                "magi.shell.exec",
                serde_json::json!({"command": "printf still-open"}),
            ),
        )
        .await;
        assert!(!is_error(&ok), "{ok}");
        assert!(ok.to_string().contains("still-open"));
    }

    #[tokio::test]
    async fn approval_mode_always_allows_all_calls_without_a_prompt() {
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_millis(300));
        let written = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.write",
                serde_json::json!({"path": "auto.txt", "content": "ok"}),
            ),
        )
        .await;
        assert!(!is_error(&written), "{written}");
        assert!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&external_session_id(&f.token_id))
                .is_empty(),
            "自动允许普通写入不应创建待确认项"
        );

        let created = rpc(
            &f,
            "tools/call",
            call("magi.fs.mkdir", serde_json::json!({"path": "auto-dir"})),
        )
        .await;
        assert!(!is_error(&created), "普通目录写入也不应再次询问: {created}");
        assert!(f.root.join("auto-dir").is_dir());
        assert!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&external_session_id(&f.token_id))
                .is_empty()
        );

        std::fs::write(f.root.join("remove-without-prompt.txt"), "remove me").unwrap();
        let removed = rpc(
            &f,
            "tools/call",
            call(
                "magi.fs.remove",
                serde_json::json!({"path": "remove-without-prompt.txt"}),
            ),
        )
        .await;
        assert!(!is_error(&removed), "开放权限应自动执行删除: {removed}");
        assert!(!f.root.join("remove-without-prompt.txt").exists());

        let command = rpc(
            &f,
            "tools/call",
            call(
                "magi.shell.exec",
                serde_json::json!({"command": "printf open-mode"}),
            ),
        )
        .await;
        assert!(!is_error(&command), "开放权限应自动执行命令: {command}");
        assert!(command.to_string().contains("open-mode"));
        let read = rpc(
            &f,
            "tools/call",
            call("magi.fs.read", serde_json::json!({"path": "auto.txt"})),
        )
        .await;
        assert!(!is_error(&read), "开放权限应自动读取: {read}");
        assert!(read.to_string().contains("ok"));
        assert!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&external_session_id(&f.token_id))
                .is_empty(),
            "开放权限调用不应留下待确认项"
        );
    }

    #[tokio::test]
    async fn ask_mode_still_prompts_for_destructive_calls() {
        let f = fixture_with_approval(ApprovalMode::Ask, Duration::from_millis(250));
        std::fs::write(f.root.join("keep.txt"), "keep").unwrap();
        let removed = rpc(
            &f,
            "tools/call",
            call("magi.fs.remove", serde_json::json!({"path": "keep.txt"})),
        )
        .await;
        assert!(is_error(&removed), "删除仍需逐次授权: {removed}");
        assert!(f.root.join("keep.txt").exists(), "未授权时不得删除文件");
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
        let f = fixture(Duration::from_secs(5));
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
        // 令牌统一开放完整目录：命令执行与下游写入类工具都在，授权方式只决定是否询问。
        assert!(names.contains(&"magi.shell.exec".to_string()));
        assert!(
            !names.contains(&"magi.shell_exec".to_string()),
            "内部名不得作为公开名重复出现"
        );
    }

    #[tokio::test]
    async fn connector_tool_digest_is_stable_for_the_same_catalog() {
        let f = fixture(Duration::from_secs(5));
        let first = web_slot_tool_digest(&f.state).expect("catalog digest");
        assert_eq!(Some(first.clone()), web_slot_tool_digest(&f.state));
        assert_eq!(first.len(), 64);
    }

    #[tokio::test]
    async fn web_slot_tool_count_matches_the_slots_own_tools_list() {
        // 连接器状态的工具数来自 GPT Web 槽位自己的目录（按槽位工具档过滤），
        // 而不是令牌的完整目录；两者都必须与各自真实的 tools/list 一致。
        let f = fixture(Duration::from_secs(5));
        let slot_principal = magi_mcp_server::ClientPrincipal {
            token_id: crate::web_slot_mcp::WEB_SLOT_PRINCIPAL_ID.to_string(),
            token_prefix: crate::web_slot_mcp::WEB_SLOT_PRINCIPAL_ID.to_string(),
            client_name: "GPT Web".to_string(),
            workspace_id: String::new(),
            profile: crate::web_slot_mcp::slot_profile_from_setting(None),
            approval_mode: ApprovalMode::Ask,
            attribution: magi_mcp_server::AttributionMode::FollowWebSlot,
        };
        let response = build_slot_mcp_server(f.state.clone())
            .handle(
                &slot_principal,
                magi_mcp_server::JsonRpcRequest {
                    jsonrpc: "2.0".to_string(),
                    id: Some(serde_json::json!(1)),
                    method: "tools/list".to_string(),
                    params: Some(serde_json::json!({})),
                },
                1,
            )
            .await
            .expect("slot tools/list responds");
        let slot_count = response["result"]["tools"].as_array().unwrap().len();
        assert_eq!(
            web_slot_tool_count(&f.state),
            u64::try_from(slot_count).ok(),
            "connector status count must come from the slot's own tools/list catalog"
        );
        let token_names = tool_names(&f).await;
        assert!(
            token_names.len() > slot_count,
            "令牌的完整目录比槽位目录多出命令执行等工具"
        );
        assert!(token_names.contains(&"magi.shell.exec".to_string()));
    }

    #[tokio::test]
    async fn open_mode_runs_downstream_read_and_write_tools_without_approval() {
        let f = fixture_with_approval(ApprovalMode::Always, Duration::from_millis(400));
        let search = rpc(
            &f,
            "tools/call",
            call("mcp.mcp__docs__search", serde_json::json!({})),
        )
        .await;
        assert!(!is_error(&search), "{search}");
        assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let publish = rpc(
            &f,
            "tools/call",
            call("mcp.mcp__docs__publish", serde_json::json!({})),
        )
        .await;
        assert!(!is_error(&publish), "{publish}");
        assert_eq!(f.mcp_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn extra_builtin_runs_through_the_same_policy_and_unknown_dynamic_names_are_refused() {
        let f = fixture(Duration::from_secs(5));
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
