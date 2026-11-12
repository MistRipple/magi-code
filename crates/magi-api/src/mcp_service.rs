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
                    input_schema: serde_json::json!({ "type": "object", "properties": {
                        "source_folder_id": {"type":"string","minLength":1,"description":"本次获授的源目录 ID；省略使用令牌或原 Turn 的默认目录"}
                    }, "additionalProperties": false }),
                });
            }
            CHANGES_REVERT => {
                return Some(ToolSchema {
                    description: "回退该客户端产生的指定待处理变更，把文件恢复到变更前的状态。需要用户在 Magi 中确认。"
                        .to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "source_folder_id": {"type":"string","minLength":1,"description":"本次获授的源目录 ID；省略使用令牌或原 Turn 的默认目录"},
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
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| {
                    Path::new(workspace.primary_source_folder().path.as_str())
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| workspace.primary_source_folder().path.to_string())
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
        self.execution_directories(principal, target)
            .ok()
            .flatten()
            .and_then(|map| map.root(None).and_then(|root| root.execution_root()).ok())
    }

    fn name_of(&self, workspace_id: &str) -> Option<String> {
        workspace_name(&self.state, workspace_id)
    }

    fn execution_directories(
        &self,
        principal: &magi_mcp_server::ClientPrincipal,
        target: &AttributionTarget,
    ) -> Result<Option<magi_core::ProjectDirectoryExecutionMap>, String> {
        if principal.attribution == magi_mcp_server::AttributionMode::Conversation {
            let current = self.state.mcp_service.conversations.bind(
                &self.state,
                principal,
                principal.session_id.as_deref().ok_or("缺少聊天上下文")?,
            )?;
            if current != *principal {
                return Err("会话目录绑定已变化".into());
            }
        }
        if principal.workspace_id.is_empty() {
            return Err("外部令牌缺少项目授权".into());
        }
        let workspace = WorkspaceId::new(&principal.workspace_id);
        let original = Some(
            principal
                .project_directories
                .clone()
                .ok_or("令牌缺少已持久化的目录授权")?,
        );
        let binding = crate::task_dispatch::validate_or_migrate_directory_binding(
            &self.state,
            Some(&workspace),
            original,
        )
        .map_err(|error| error.message().to_string())?
        .ok_or("项目缺少目录授权")?;
        let session_id = match target {
            AttributionTarget::ExternalSession { token_id } => principal
                .session_id
                .as_deref()
                .map(SessionId::new)
                .unwrap_or_else(|| external_session_id(token_id)),
        };
        self.state
            .workspace_registry
            .validate_directory_binding(&binding)
            .map_err(|error| error.to_string())?;
        let map = magi_core::ProjectDirectoryExecutionMap::from_binding(&binding, |grant| {
            let source = self
                .state
                .workspace_registry
                .resolve_bound_source_directory(&binding, Some(&grant.source_folder_id))?;
            self.state
                .session_execution_root_for_source(&session_id, &source)
                .map_err(|error| magi_core::DomainError::Validation {
                    message: error.message().to_string(),
                })
        })
        .map_err(|error| error.to_string())?;
        Ok(Some(map))
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
                session_id: event.session_id.clone(),
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
        if let AuditOutcome::Denied { reason } = &event.outcome {
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
                    "session_id": event.session_id,
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
            // 下游工具的副作用 Magi 看不见：非只读调用只有完全访问模式自动放行。
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
        let is_alive = liveness(&self.state, &self.tokens, invocation);
        if !is_alive() {
            return cancelled_outcome();
        }
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
                        .and_then(|()| snapshot.pending_tool_changes())
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
                let reverted = tokio::task::spawn_blocking(move || {
                    if !is_alive() {
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

    fn resolve_session(&self, invocation: &ToolInvocation) -> Result<SessionId, String> {
        if let Some(id) = &invocation.principal.session_id {
            return self
                .state
                .mcp_service
                .conversations
                .bind(&self.state, &invocation.principal, id)
                .map(|_| SessionId::new(id));
        }
        match &invocation.attribution {
            AttributionTarget::ExternalSession { token_id } => ensure_external_session(
                &self.state,
                token_id,
                &invocation.principal.client_name,
                &invocation.principal.workspace_id,
            ),
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

    fn project_execution_map(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<Option<magi_core::ProjectDirectoryExecutionMap>, String> {
        let map = RegistryWorkspaces {
            state: self.state.clone(),
        }
        .execution_directories(&invocation.principal, &invocation.attribution)?;
        if map != invocation.project_directory_map {
            return Err("调用期间项目执行映射已变化".into());
        }
        Ok(map)
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
    let session_id = match event
        .session_id
        .as_deref()
        .map(|id| Ok(SessionId::new(id)))
        .unwrap_or_else(|| {
            ensure_external_session(
                state,
                &event.token_id,
                &event.client_name,
                &event.workspace_id,
            )
        }) {
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

pub(crate) struct ApprovalOwner {
    pub token_id: String,
    pub client_name: String,
    pub token_prefix: String,
    pub workspace_id: String,
    pub session_id: SessionId,
}

/// 当前所有可能持有外部待审批的调用方。跨会话待确认入口只扫描它们。
pub(crate) fn approval_owners(state: &ApiState) -> Vec<ApprovalOwner> {
    state
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
        .chain(
            state
                .mcp_service
                .conversations
                .records()
                .into_iter()
                .filter_map(|session| {
                    let token = state
                        .mcp_service
                        .list_tokens()
                        .into_iter()
                        .find(|token| token.token_id == session.token_id)?;
                    Some(ApprovalOwner {
                        session_id: SessionId::new(session.session_id),
                        token_id: token.token_id,
                        client_name: token.client_name,
                        token_prefix: token.prefix,
                        workspace_id: session.directories?.workspace_id.to_string(),
                    })
                }),
        )
        .collect::<Vec<_>>()
}

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
    let principal = invocation.principal.clone();
    let class = invocation.class;
    let directories = invocation.project_directory_map.clone();
    let cancellation = invocation.cancellation.clone();
    move || {
        !cancellation.is_cancelled()
            && (RegistryWorkspaces {
                state: state.clone(),
            })
            .execution_directories(&principal, &attribution)
            .is_ok_and(|current| current == directories)
            && match &attribution {
                AttributionTarget::ExternalSession { .. } => {
                    tokens.records().iter().any(|record| {
                        record.token_id == token_id
                            && record.is_active(UtcMillis::now().0)
                            && Some(&record.hash) == token_hash.as_ref()
                            && (principal.approval_mode.requires_approval(class)
                                || !record.approval_mode.requires_approval(class))
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
    fn start_session(
        &self,
        principal: &magi_mcp_server::ClientPrincipal,
        arguments: &Value,
    ) -> InvocationOutcome {
        let Some(conversation) = arguments.get("conversation_id").and_then(Value::as_str) else {
            return error_outcome("缺少 conversation_id");
        };
        let title = arguments.get("title").and_then(Value::as_str).unwrap_or("");
        if !self.tokens.records().iter().any(|record| {
            record.token_id == principal.token_id && record.is_active(UtcMillis::now().0)
        }) {
            return error_outcome("客户端授权已失效");
        }
        let record =
            match self
                .state
                .mcp_service
                .conversations
                .start(principal, conversation, title)
            {
                Ok(record) => record,
                Err(error) => return error_outcome(error),
            };
        crate::external_turns::publish_changed(&self.state, &SessionId::new(&record.session_id));
        if record.denied {
            return error_outcome("用户已拒绝此会话的目录授权");
        }
        if record.directories.is_some() {
            return match self.bind_session(principal, &record.session_id) {
                Ok(bound) => InvocationOutcome { is_error: false, text: serde_json::json!({"status":"ready","magi_session_id":record.session_id,"directories":bound.project_directories,"message":"目录已选定。本聊天后续工具调用复用此 magi_session_id，无需再次选择目录。"}).to_string() },
                Err(error) => error_outcome(error),
            };
        }
        InvocationOutcome { is_error: false, text: serde_json::json!({"status":"awaiting_directory","magi_session_id":record.session_id,"message":"请用户在本机 Magi 的目录选择提示中选择本会话的执行目录。选择后使用相同 conversation_id 再调用本工具确认；不要创建新会话，不要连续轮询。"}).to_string() }
    }

    fn bind_session(
        &self,
        principal: &magi_mcp_server::ClientPrincipal,
        session_id: &str,
    ) -> Result<magi_mcp_server::ClientPrincipal, String> {
        self.state
            .mcp_service
            .conversations
            .bind(&self.state, principal, session_id)
    }

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
        let structured_git = BuiltinToolName::from_name(internal_name)
            .is_some_and(|tool| tool.requires_repository());
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
                // 仅结构化 Git 工具可以经 GitService 修改元数据；通用 PathGuard
                // 只负责校验工作树、gitdir/commondir 的范围与符号链接。
                magi_permissions::PathAccessKind::Write if structured_git => PathAccess::Read,
                magi_permissions::PathAccessKind::Write => PathAccess::Write,
            },
        })
        .collect()
    }

    fn invoke<'a>(&'a self, invocation: ToolInvocation) -> BoxFuture<'a, InvocationOutcome> {
        Box::pin(async move {
            let call_id = next_call_id(&invocation.principal.token_id);
            if let Err(error) = self.resolve_session(&invocation) {
                return error_outcome(error);
            }
            let workspace_id = (!invocation.principal.workspace_id.is_empty())
                .then(|| WorkspaceId::new(invocation.principal.workspace_id.clone()));
            let turns = &self.state.mcp_service.external_turns;
            let target = match &invocation.attribution {
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
                        Ok(turn_id) => Some((session_id, Some(turn_id))),
                        Err(error) => {
                            // 记录失败不应让外部客户端的调用整体不可用：照常执行，只是这次没有展示。
                            tracing::warn!(%error, "外部调用无法写入外部会话时间线");
                            None
                        }
                    }
                }
            };
            let external_session_id = target.as_ref().map(|(session_id, _)| session_id.clone());
            let save_content = self.state.mcp_service.save_content();
            let recorded_arguments = if save_content {
                invocation.arguments.to_string()
            } else {
                redact_arguments(&invocation.arguments)
            };
            let tool_name = invocation.internal_name.clone();
            let writer = target.as_ref().map(|(session_id, turn_id)| {
                magi_conversation_runtime::session_writeback::ExternalToolItemWriter {
                    session_store: &self.state.session_store,
                    event_bus: &self.state.event_bus,
                    session_id,
                    workspace_id: &workspace_id,
                    turn_id: turn_id.as_deref(),
                }
            });
            if let Some(writer) = &writer
                && let Err(error) = writer.started(&call_id, &tool_name, &recorded_arguments)
            {
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
            let project_directory_map = match self.project_execution_map(&invocation) {
                Ok(map) => map,
                Err(error) => return error_outcome(error),
            };
            let root = if let Some(map) = project_directory_map.as_ref() {
                let context = magi_tool_runtime::ToolExecutionContext {
                    workspace_id: workspace_id.clone(),
                    project_directory_map: Some(map.clone()),
                    ..Default::default()
                };
                match context.for_tool_arguments(&invocation.arguments.to_string()) {
                    Ok(context) => context.working_directory,
                    Err(error) => return error_outcome(error.to_string()),
                }
            } else {
                RegistryWorkspaces {
                    state: self.state.clone(),
                }
                .root_for(&invocation.principal, &invocation.attribution)
            };
            let Some(root) = root else {
                return error_outcome("调用目录不可用");
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
            let snapshots = if workspace_id.is_none() || matches!(class, Some(ToolClass::Read)) {
                Vec::new()
            } else {
                let roots = match project_directory_map
                    .as_ref()
                    .map(|map| {
                        map.roots
                            .iter()
                            .map(|root| root.execution_root())
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()
                {
                    Ok(roots) => roots.unwrap_or_else(|| vec![root.clone()]),
                    Err(error) => return error_outcome(error.to_string()),
                };
                let mut snapshots = Vec::new();
                for root in roots {
                    match self.state.ensure_snapshot_session(&session_id, &root).await {
                        Ok(snapshot) => snapshots.push(snapshot),
                        Err(_) => return error_outcome("变更账本不可用，已拒绝执行写入类工具"),
                    }
                }
                snapshots
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
                    snapshots,
                    project_directory_map,
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
    if !invocation
        .principal
        .approval_mode
        .requires_approval(invocation.class)
    {
        return Ok(());
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

struct ExternalGitExecution {
    state: ApiState,
    session_id: SessionId,
    _lock: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for ExternalGitExecution {
    fn drop(&mut self) {
        self.state
            .release_session_git_execution_lease(&self.session_id);
    }
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
    snapshots: Vec<Arc<magi_snapshot::SnapshotSession>>,
    project_directory_map: Option<magi_core::ProjectDirectoryExecutionMap>,
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

    if invocation.cancellation.is_cancelled() {
        return cancelled_outcome();
    }
    // 令牌按本次工具类别预授权；路径越界等硬性策略仍由执行层拒绝。
    let mut granted = !invocation
        .principal
        .approval_mode
        .requires_approval(invocation.class);
    if invocation.requires_approval {
        if let Err((payload, _)) = approve(call_id) {
            return error_outcome(payload);
        }
        granted = true;
    }
    if !is_alive() {
        return cancelled_outcome();
    }
    // Consent must settle before acquiring a repository lease. The guard lives in
    // this blocking execution, so disconnecting the HTTP request cannot release a
    // lease while the actual Git process is still running.
    let _git_execution = if BuiltinToolName::from_name(&invocation.internal_name)
        .is_some_and(|tool| tool.requires_repository() && tool.is_write_operation())
        && let Some(workspace_id) = workspace_id
    {
        let target = invocation
            .arguments
            .get("repositoryPath")
            .and_then(Value::as_str)
            .map(|path| {
                magi_core::HostPath::resolve_native_input(path, Some(root), None)
                    .map(magi_core::HostPath::into_path_buf)
            })
            .transpose();
        let target = match target {
            Ok(path) => path.unwrap_or_else(|| root.to_path_buf()),
            Err(error) => return error_outcome(error.to_string()),
        };
        let handle = tokio::runtime::Handle::current();
        let prepared = handle.block_on(async {
            tokio::select! {
                prepared = async {
                    let lock = state.lock_session_turn(session_id).await;
                    let guard = ExternalGitExecution { state: state.clone(), session_id: session_id.clone(), _lock: lock };
                    state.ensure_session_code_context_for_root(session_id, workspace_id, &target).await?;
                    Ok::<_, crate::errors::ApiError>(guard)
                } => prepared,
                _ = async {
                    loop {
                        if !is_alive() { break; }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => Err(crate::errors::ApiError::Conflict("Git 调用已取消或授权已失效".into())),
            }
        });
        match prepared {
            Ok(guard) => Some(guard),
            Err(error) => return error_outcome(format!("无法准备 Git 执行: {}", error.message())),
        }
    } else {
        None
    };
    if !is_alive() {
        return cancelled_outcome();
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
        project_directory_map,
        snapshots,
        access_profile,
        snapshot: None,
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
        if let Err((payload, _)) = approve(&format!("{call_id}-safety")) {
            return error_outcome(payload);
        }
        if !is_alive() {
            return cancelled_outcome();
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

fn access_profile_for(profile: Profile) -> AccessProfile {
    match profile {
        Profile::ReadOnly => AccessProfile::ReadOnly,
        Profile::Edit | Profile::EditTrusted | Profile::Exec => AccessProfile::Restricted,
    }
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

#[cfg(test)]
mod tests;
