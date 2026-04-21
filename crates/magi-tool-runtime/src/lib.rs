use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId, TaskId,
    ToolCallId, UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::{
    GovernanceDecision, GovernanceService, ToolExecutionRequest, ToolKind,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, RwLock},
};

mod builtin;
mod policy;
use builtin::{infer_execution_status, NormalizedBuiltinTool};
use policy::WriteProtectionClaim;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinToolName {
    // ── 文件系统 ──
    FileRead,
    FileWrite,
    FilePatch,
    FileRemove,
    FileMkdir,
    FileCopy,
    FileMove,
    // ── 搜索 ──
    SearchText,
    SearchSemantic,
    // ── Shell / 进程 ──
    ShellExec,
    ProcessInspect,
    // ── Diff ──
    DiffPreview,
    // ── Web ──
    WebSearch,
    WebFetch,
    // ── 可视化 ──
    MermaidDiagram,
    // ── 知识库 ──
    KnowledgeQuery,
    // ── 编排 ──
    WorkerSendMessage,
    TodoSplit,
    TodoList,
    TodoUpdate,
    TodoClaimNext,
    ContextCompact,
    // ── Skill ──
    SkillApply,
}

impl BuiltinToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FileRead => "file.read",
            Self::FileWrite => "file.write",
            Self::FilePatch => "file.patch",
            Self::FileRemove => "file.remove",
            Self::FileMkdir => "file.mkdir",
            Self::FileCopy => "file.copy",
            Self::FileMove => "file.move",
            Self::SearchText => "search.text",
            Self::SearchSemantic => "search.semantic",
            Self::ShellExec => "shell.exec",
            Self::ProcessInspect => "process.inspect",
            Self::DiffPreview => "diff.preview",
            Self::WebSearch => "web.search",
            Self::WebFetch => "web.fetch",
            Self::MermaidDiagram => "mermaid.diagram",
            Self::KnowledgeQuery => "knowledge.query",
            Self::WorkerSendMessage => "orchestration.worker_send_message",
            Self::TodoSplit => "orchestration.todo_split",
            Self::TodoList => "orchestration.todo_list",
            Self::TodoUpdate => "orchestration.todo_update",
            Self::TodoClaimNext => "orchestration.todo_claim_next",
            Self::ContextCompact => "orchestration.context_compact",
            Self::SkillApply => "skill.apply",
        }
    }

    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "file.read" | "file_view" => Some(Self::FileRead),
            "file.write" | "file_create" => Some(Self::FileWrite),
            "file.patch" | "file_edit" | "file_insert" => Some(Self::FilePatch),
            "file.remove" | "file_remove" => Some(Self::FileRemove),
            "file.mkdir" => Some(Self::FileMkdir),
            "file.copy" => Some(Self::FileCopy),
            "file.move" => Some(Self::FileMove),
            "search.text" | "code_search_regex" => Some(Self::SearchText),
            "search.semantic" | "code_search_semantic" => Some(Self::SearchSemantic),
            "shell.exec" | "shell" => Some(Self::ShellExec),
            "process.inspect" => Some(Self::ProcessInspect),
            "diff.preview" => Some(Self::DiffPreview),
            "web.search" | "web_search" => Some(Self::WebSearch),
            "web.fetch" | "web_fetch" => Some(Self::WebFetch),
            "mermaid.diagram" | "mermaid_diagram" => Some(Self::MermaidDiagram),
            "knowledge.query" | "project_knowledge_query" => Some(Self::KnowledgeQuery),
            "orchestration.worker_send_message" | "worker_send_message" => Some(Self::WorkerSendMessage),
            "orchestration.todo_split" | "todo_split" => Some(Self::TodoSplit),
            "orchestration.todo_list" | "todo_list" => Some(Self::TodoList),
            "orchestration.todo_update" | "todo_update" => Some(Self::TodoUpdate),
            "orchestration.todo_claim_next" | "todo_claim_next" => Some(Self::TodoClaimNext),
            "orchestration.context_compact" | "context_compact" => Some(Self::ContextCompact),
            "skill.apply" | "skill_apply" => Some(Self::SkillApply),
            _ => None,
        }
    }

    pub fn is_orchestration(&self) -> bool {
        matches!(
            self,
            Self::WorkerSendMessage
                | Self::TodoSplit
                | Self::TodoList
                | Self::TodoUpdate
                | Self::TodoClaimNext
                | Self::ContextCompact
        )
    }

    pub fn is_write_operation(&self) -> bool {
        matches!(
            self,
            Self::FileWrite
                | Self::FilePatch
                | Self::FileRemove
                | Self::FileMkdir
                | Self::FileCopy
                | Self::FileMove
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuiltinToolSpec {
    pub name: String,
    pub risk_level: RiskLevel,
    pub approval_requirement: ApprovalRequirement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinToolAccessMode {
    ReadOnly,
    MaybeWrite,
    ExplicitWrite,
}

impl BuiltinToolAccessMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::MaybeWrite => "maybe_write",
            Self::ExplicitWrite => "explicit_write",
        }
    }

    fn is_writeful(&self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "read" | "read_only" | "readonly" => Some(Self::ReadOnly),
            "maybe" | "maybe_write" | "maybewrite" => Some(Self::MaybeWrite),
            "write" | "explicit_write" | "explicitwrite" => Some(Self::ExplicitWrite),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionInput {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub input: String,
    pub approval_requirement: ApprovalRequirement,
    pub risk_level: RiskLevel,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionContext {
    pub worker_id: Option<WorkerId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionContextQuery {
    pub worker_id: Option<WorkerId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProtectionScope {
    pub workspace_id: Option<WorkspaceId>,
    pub task_id: Option<TaskId>,
    pub working_directory: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutionOutput {
    pub tool_call_id: ToolCallId,
    pub status: ExecutionResultStatus,
    pub payload: String,
    pub governance: GovernanceDecision,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionPolicy {
    pub source_skill_ids: Vec<String>,
    pub allowed_tool_names: Vec<String>,
    pub denied_tool_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolInvocationRecord {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    pub context: ToolExecutionContext,
    pub status: ExecutionResultStatus,
    pub payload: String,
    pub created_at: UtcMillis,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolExecutionSummary {
    pub total_invocations: usize,
    pub successful_invocations: usize,
    pub blocked_invocations: usize,
    pub failed_invocations: usize,
}

pub trait BuiltinTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn execute(&self, input: &str) -> String;
    fn spec(&self) -> BuiltinToolSpec;
}

#[derive(Clone)]
pub struct ToolRegistry {
    governance: Arc<GovernanceService>,
    event_bus: Arc<InMemoryEventBus>,
    builtin_tools: HashMap<String, Arc<dyn BuiltinTool>>,
    invocations: Arc<RwLock<Vec<ToolInvocationRecord>>>,
    active_write_claims: Arc<RwLock<HashMap<ToolCallId, WriteProtectionClaim>>>,
}

impl ToolRegistry {
    pub fn new(governance: Arc<GovernanceService>, event_bus: Arc<InMemoryEventBus>) -> Self {
        Self {
            governance,
            event_bus,
            builtin_tools: HashMap::new(),
            invocations: Arc::new(RwLock::new(Vec::new())),
            active_write_claims: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn register_builtin(&mut self, tool: Arc<dyn BuiltinTool>) {
        self.builtin_tools.insert(tool.name().to_string(), tool);
    }

    pub fn register_default_builtins(&mut self) {
        let tools: &[(BuiltinToolName, RiskLevel, ApprovalRequirement)] = &[
            // 文件系统
            (BuiltinToolName::FileRead, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::FileWrite, RiskLevel::Medium, ApprovalRequirement::None),
            (BuiltinToolName::FilePatch, RiskLevel::Medium, ApprovalRequirement::None),
            (BuiltinToolName::FileRemove, RiskLevel::High, ApprovalRequirement::Required),
            (BuiltinToolName::FileMkdir, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::FileCopy, RiskLevel::Medium, ApprovalRequirement::None),
            (BuiltinToolName::FileMove, RiskLevel::Medium, ApprovalRequirement::None),
            // 搜索
            (BuiltinToolName::SearchText, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::SearchSemantic, RiskLevel::Low, ApprovalRequirement::None),
            // Shell / 进程
            (BuiltinToolName::ShellExec, RiskLevel::High, ApprovalRequirement::Required),
            (BuiltinToolName::ProcessInspect, RiskLevel::Medium, ApprovalRequirement::None),
            // Diff
            (BuiltinToolName::DiffPreview, RiskLevel::Low, ApprovalRequirement::None),
            // Web
            (BuiltinToolName::WebSearch, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::WebFetch, RiskLevel::Low, ApprovalRequirement::None),
            // 可视化
            (BuiltinToolName::MermaidDiagram, RiskLevel::Low, ApprovalRequirement::None),
            // 知识库
            (BuiltinToolName::KnowledgeQuery, RiskLevel::Low, ApprovalRequirement::None),
            // 编排
            (BuiltinToolName::WorkerSendMessage, RiskLevel::Medium, ApprovalRequirement::None),
            (BuiltinToolName::TodoSplit, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::TodoList, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::TodoUpdate, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::TodoClaimNext, RiskLevel::Low, ApprovalRequirement::None),
            (BuiltinToolName::ContextCompact, RiskLevel::Medium, ApprovalRequirement::None),
            // Skill
            (BuiltinToolName::SkillApply, RiskLevel::Low, ApprovalRequirement::None),
        ];
        for &(name, risk, approval) in tools {
            self.register_builtin(Arc::new(NormalizedBuiltinTool::new(name, risk, approval)));
        }
    }

    pub fn builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        self.builtin_tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn builtin_access_mode(&self, tool_name: &str) -> Option<BuiltinToolAccessMode> {
        self.builtin_tools.get(tool_name).map(|_| {
            match BuiltinToolName::from_str(tool_name) {
                Some(name) if name == BuiltinToolName::ShellExec => BuiltinToolAccessMode::MaybeWrite,
                Some(name) if name.is_write_operation() => BuiltinToolAccessMode::ExplicitWrite,
                _ => BuiltinToolAccessMode::ReadOnly,
            }
        })
    }

    pub fn execute(&self, input: ToolExecutionInput) -> ToolExecutionOutput {
        self.execute_with_context(input, ToolExecutionContext::default())
    }

    pub fn execute_with_context(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
    ) -> ToolExecutionOutput {
        self.execute_with_policy(input, context, &ToolExecutionPolicy::default())
    }

    pub fn execute_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        if let Some(output) = self.enforce_execution_policy(&input, policy) {
            self.record_invocation(&input, &context, &output);
            return output;
        }

        let governance = self.governance.evaluate_tool_request(&ToolExecutionRequest {
            tool_name: input.tool_name.clone(),
            tool_kind: input.tool_kind.clone(),
            risk_level: input.risk_level,
            approval_requirement: input.approval_requirement,
        });

        let output = if !governance.allowed {
            ToolExecutionOutput {
                tool_call_id: input.tool_call_id.clone(),
                status: if governance.requires_approval {
                    ExecutionResultStatus::NeedsApproval
                } else {
                    ExecutionResultStatus::Rejected
                },
                payload: governance
                    .reason
                    .clone()
                    .unwrap_or_else(|| "工具调用被阻断".to_string()),
                governance,
            }
        } else {
            match self.builtin_tools.get(&input.tool_name) {
                Some(tool) => {
                    let access_mode = self.resolve_access_mode(&input);
                    let write_guard = match self.acquire_write_guard(&input, &context, access_mode) {
                        Ok(guard) => guard,
                        Err(output) => {
                            self.record_invocation(&input, &context, &output);
                            return output;
                        }
                    };
                    let payload = tool.execute(&input.input);
                    drop(write_guard);
                    ToolExecutionOutput {
                        tool_call_id: input.tool_call_id.clone(),
                        status: infer_execution_status(&payload),
                        payload,
                        governance,
                    }
                }
                None => ToolExecutionOutput {
                    tool_call_id: input.tool_call_id.clone(),
                    status: ExecutionResultStatus::Failed,
                    payload: format!("未注册的工具: {}", input.tool_name),
                    governance,
                },
            }
        };

        self.record_invocation(&input, &context, &output);
        output
    }

    pub fn invocations(&self) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .clone()
    }

    pub fn summary(&self) -> ToolExecutionSummary {
        self.summary_for_query(&ToolExecutionContextQuery::default())
    }

    pub fn query_invocations(&self, query: &ToolExecutionContextQuery) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .iter()
            .filter(|record| query.worker_id.as_ref().is_none_or(|id| record.context.worker_id.as_ref() == Some(id)))
            .filter(|record| query.task_id.as_ref().is_none_or(|id| record.context.task_id.as_ref() == Some(id)))
            .filter(|record| query.session_id.as_ref().is_none_or(|id| record.context.session_id.as_ref() == Some(id)))
            .filter(|record| query.workspace_id.as_ref().is_none_or(|id| record.context.workspace_id.as_ref() == Some(id)))
            .cloned()
            .collect()
    }

    pub fn summary_for_query(&self, query: &ToolExecutionContextQuery) -> ToolExecutionSummary {
        let invocations = self.query_invocations(query);
        let invocations = self
            .summarize_invocations(&invocations);
        invocations
    }

    fn summarize_invocations(&self, invocations: &[ToolInvocationRecord]) -> ToolExecutionSummary {
        let total_invocations = invocations.len();
        let successful_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Succeeded)
            .count();
        let blocked_invocations = invocations
            .iter()
            .filter(|record| {
                matches!(
                    record.status,
                    ExecutionResultStatus::NeedsApproval | ExecutionResultStatus::Rejected
                )
            })
            .count();
        let failed_invocations = invocations
            .iter()
            .filter(|record| record.status == ExecutionResultStatus::Failed)
            .count();
        ToolExecutionSummary {
            total_invocations,
            successful_invocations,
            blocked_invocations,
            failed_invocations,
        }
    }

    fn record_invocation(
        &self,
        input: &ToolExecutionInput,
        context: &ToolExecutionContext,
        output: &ToolExecutionOutput,
    ) {
        let record = ToolInvocationRecord {
            tool_call_id: input.tool_call_id.clone(),
            tool_name: input.tool_name.clone(),
            tool_kind: input.tool_kind.clone(),
            context: context.clone(),
            status: output.status,
            payload: output.payload.clone(),
            created_at: UtcMillis::now(),
        };
        self.invocations
            .write()
            .expect("tool invocation write lock poisoned")
            .push(record.clone());
        self.publish_with_category(
            "tool.invoked",
            EventCategory::Audit,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                task_id: record.context.task_id.clone(),
                ..EventContext::default()
            },
            EventId::new(format!("tool-call-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
        self.publish_with_category(
            "tool.usage.recorded",
            EventCategory::Usage,
            EventContext {
                workspace_id: record.context.workspace_id.clone(),
                session_id: record.context.session_id.clone(),
                mission_id: None,
                assignment_id: None,
                task_id: record.context.task_id.clone(),
            },
            EventId::new(format!("tool-usage-{}", record.tool_call_id)),
            serde_json::json!({
                "tool_call_id": record.tool_call_id.to_string(),
                "tool_name": record.tool_name,
                "tool_kind": format!("{:?}", record.tool_kind),
                "status": format!("{:?}", record.status),
                "risk_level": format!("{:?}", input.risk_level),
                "approval_requirement": format!("{:?}", input.approval_requirement),
                "worker_id": record.context.worker_id.as_ref().map(ToString::to_string),
                "task_id": record.context.task_id.as_ref().map(ToString::to_string),
                "session_id": record.context.session_id.as_ref().map(ToString::to_string),
                "workspace_id": record.context.workspace_id.as_ref().map(ToString::to_string)
            }),
        );
    }
}

impl ToolRegistry {
    fn publish_with_category(
        &self,
        event_type: &str,
        category: EventCategory,
        context: EventContext,
        event_id: EventId,
        payload: serde_json::Value,
    ) {
        let envelope = match category {
            EventCategory::Domain => EventEnvelope::domain(event_id, event_type, payload),
            EventCategory::Audit => EventEnvelope::audit(event_id, event_type, payload),
            EventCategory::Usage => EventEnvelope::usage(event_id, event_type, payload),
            EventCategory::Projection => EventEnvelope::projection(event_id, event_type, payload),
            EventCategory::System => EventEnvelope::system(event_id, event_type, payload),
        };
        let _ = self.event_bus.publish(envelope.with_context(context));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_core::{
        ApprovalRequirement, RiskLevel, SessionId, TaskId, ToolCallId, WorkerId, WorkspaceId,
    };
    use magi_governance::{DecisionPhase, GovernanceOutcome};
    use serde_json::Value;
    use std::{
        fs,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}-{}", name, std::process::id(), suffix));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn file_read_supports_raw_path_and_directory_listing() {
        let root = unique_temp_dir("magi-tool-file-read");
        let file_path = root.join("hello.txt");
        fs::write(&file_path, "hello\nworld").expect("write file");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-file-read"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file_path.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "file.read");
        assert_eq!(payload["access_mode"], "read_only");
        assert_eq!(payload["mode"], "file");
        assert_eq!(payload["truncated"], false);
        assert!(payload["content"].as_str().expect("content").contains("hello"));

        let dir_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-file-read-dir"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "path": root.to_string_lossy(),
                    "max_bytes": 8
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(dir_output.status, ExecutionResultStatus::Succeeded);
        let dir_payload: Value = serde_json::from_str(&dir_output.payload).expect("dir payload json");
        assert_eq!(dir_payload["mode"], "directory");
        assert_eq!(dir_payload["entries"].as_array().expect("entries").len(), 1);
    }

    #[test]
    fn builtin_execution_emits_usage_event_and_updates_ledger() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-usage"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "/tmp/nonexistent".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);

        let snapshot = event_bus.snapshot();
        let usage_events = snapshot
            .recent_events
            .iter()
            .filter(|event| event.category == EventCategory::Usage)
            .collect::<Vec<_>>();
        assert!(!usage_events.is_empty());
        let usage_payload = &usage_events[0].payload;
        assert_eq!(usage_payload["tool_name"], "file.read");
        assert_eq!(usage_payload["status"], "Failed");
        assert_eq!(usage_payload["risk_level"], "Low");

        let ledger_status = event_bus.audit_usage_ledger_status();
        assert!(ledger_status.usage_count >= 1);
        assert_eq!(ledger_status.audit_count, 1);
    }

    #[test]
    fn search_text_supports_json_input() {
        let root = unique_temp_dir("magi-tool-search");
        fs::create_dir_all(root.join("nested")).expect("nested");
        fs::write(root.join("nested").join("one.txt"), "alpha\nneedle\nbeta").expect("write");
        fs::write(root.join("two.txt"), "needle here too").expect("write");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-search"),
                tool_name: BuiltinToolName::SearchText.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "root": root.to_string_lossy(),
                    "query": "needle",
                    "limit": 10,
                    "case_sensitive": true
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "search.text");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(payload["returned_matches"].as_u64().expect("returned matches") >= 2);
        assert!(!payload["matches"].as_array().expect("matches").is_empty());
    }

    #[test]
    fn shell_exec_runs_and_reports_failure_semantics() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf hello".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "shell.exec");
        assert_eq!(payload["access_mode"], "maybe_write");
        assert_eq!(payload["stdout"], "hello");

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-blocked"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf blocked".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::NeedsApproval);
    }

    #[test]
    fn shell_exec_blocks_conflicting_write_scope_until_guard_drops() {
        let root = unique_temp_dir("magi-tool-shell-write");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-write")),
            session_id: Some(SessionId::new("session-write")),
            workspace_id: Some(WorkspaceId::new("workspace-write")),
        };
        let guarded_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-write-guard"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf guarded",
                "cwd": root.to_string_lossy(),
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let write_guard = tool_registry
            .acquire_write_guard(
                &guarded_input,
                &context,
                BuiltinToolAccessMode::ExplicitWrite,
            )
            .expect("guard acquisition")
            .expect("writeful guard");

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-write-blocked"),
                ..guarded_input.clone()
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
        let blocked_payload: Value =
            serde_json::from_str(&blocked.payload).expect("blocked payload json");
        assert_eq!(blocked_payload["tool"], "shell.exec");
        assert_eq!(blocked_payload["access_mode"], "explicit_write");
        assert!(
            blocked_payload["error"]
                .as_str()
                .expect("blocked error")
                .contains("并发写冲突")
        );

        drop(write_guard);

        let allowed = tool_registry.execute_with_policy(
            guarded_input,
            context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(allowed.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn shell_exec_blocks_same_working_directory_across_different_contexts() {
        let root = unique_temp_dir("magi-tool-shell-workdir");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let guarded_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-a")),
            session_id: Some(SessionId::new("session-a")),
            workspace_id: Some(WorkspaceId::new("workspace-a")),
        };
        let other_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-b")),
            session_id: Some(SessionId::new("session-b")),
            workspace_id: Some(WorkspaceId::new("workspace-b")),
        };
        let guarded_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-shell-workdir-guard"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf guarded",
                "cwd": root.to_string_lossy(),
                "access_mode": "maybe_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let write_guard = tool_registry
            .acquire_write_guard(
                &guarded_input,
                &guarded_context,
                BuiltinToolAccessMode::MaybeWrite,
            )
            .expect("guard acquisition")
            .expect("writeful guard");

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-workdir-blocked"),
                ..guarded_input.clone()
            },
            other_context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked.status, ExecutionResultStatus::Rejected);
        let blocked_payload: Value =
            serde_json::from_str(&blocked.payload).expect("blocked payload json");
        assert_eq!(blocked_payload["access_mode"], "maybe_write");
        assert!(
            blocked_payload["error"]
                .as_str()
                .expect("blocked error")
                .contains("并发写冲突")
        );

        drop(write_guard);
    }

    #[test]
    fn process_inspect_reports_current_process() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let current_pid = std::process::id();
        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process"),
                tool_name: BuiltinToolName::ProcessInspect.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: current_pid.to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "process.inspect");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(payload["matches"].as_array().expect("matches").iter().any(|item| {
            item["pid"]
                .as_u64()
                .map(|pid| pid as u32 == current_pid)
                .unwrap_or(false)
        }));
    }

    #[test]
    fn diff_preview_reports_text_deltas() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-diff"),
                tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "before": "line1\nsame\nold",
                    "after": "line1\nsame\nnew"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], "diff.preview");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(payload["preview"]
            .as_str()
            .expect("preview")
            .contains("+new"));
        assert!(payload["preview"]
            .as_str()
            .expect("preview")
            .contains("-old"));
    }

    #[test]
    fn builtin_invocation_emits_usage_event_and_updates_ledger() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();
        let missing_path = unique_temp_dir("magi-tool-usage").join("missing.txt");

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-usage"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: missing_path.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let status = event_bus.audit_usage_ledger_status();
        assert_eq!(status.audit_count, 1);
        assert_eq!(status.usage_count, 1);
        let snapshot = event_bus.audit_usage_ledger_snapshot();
        assert_eq!(snapshot.usage_entries.len(), 1);
        assert_eq!(snapshot.usage_entries[0].event_type, "tool.usage.recorded");
        let usage_payload = snapshot.usage_entries[0].payload.clone();
        assert_eq!(usage_payload["tool_name"], "file.read");
        assert_eq!(usage_payload["status"], "Failed");
        assert_eq!(usage_payload["risk_level"], "Low");
    }

    // ── T-204: governance / summary / usage 三者一致性验证 ──────────────────

    #[test]
    fn governance_blocked_invocations_appear_in_summary_and_events() {
        // ShellExec is registered as High risk + Required approval, so default
        // GovernanceService (auto_allow_max_risk=Medium) will block it.
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        // 1) Successful invocation: file.read (Low risk, no approval needed)
        let root = unique_temp_dir("magi-tool-gov-summary");
        let file_path = root.join("ok.txt");
        fs::write(&file_path, "content").expect("write file");

        let ctx = ToolExecutionContext {
            worker_id: Some(WorkerId::new("worker-gov")),
            task_id: Some(TaskId::new("todo-gov")),
            session_id: Some(SessionId::new("session-gov")),
            workspace_id: Some(WorkspaceId::new("workspace-gov")),
        };

        let ok_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-gov-ok"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file_path.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(ok_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(ok_output.governance.outcome, GovernanceOutcome::Allowed);

        // 2) Governance-blocked invocation: shell.exec (High risk)
        let blocked_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-gov-blocked"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf blocked".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(blocked_output.status, ExecutionResultStatus::NeedsApproval);
        assert_eq!(
            blocked_output.governance.outcome,
            GovernanceOutcome::NeedsApproval
        );

        // 3) Failed invocation: file.read on nonexistent path
        let fail_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-gov-fail"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: root.join("no-such-file.txt").to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(fail_output.status, ExecutionResultStatus::Failed);
        assert_eq!(fail_output.governance.outcome, GovernanceOutcome::Allowed);

        // ── Verify summary reflects all three outcomes ──
        let summary = tool_registry.summary();
        assert_eq!(summary.total_invocations, 3);
        assert_eq!(summary.successful_invocations, 1);
        assert_eq!(summary.blocked_invocations, 1);
        assert_eq!(summary.failed_invocations, 1);

        // ── Verify event bus has matching audit + usage events ──
        let snapshot = event_bus.snapshot();
        let audit_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
            .collect();
        assert_eq!(audit_events.len(), 3);

        let usage_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
            .collect();
        assert_eq!(usage_events.len(), 3);

        // Verify the blocked event carries NeedsApproval status
        let blocked_usage = usage_events
            .iter()
            .find(|e| e.payload["tool_call_id"] == "tc-gov-blocked")
            .expect("blocked usage event");
        assert_eq!(blocked_usage.payload["status"], "NeedsApproval");
        assert_eq!(blocked_usage.payload["risk_level"], "High");

        // Verify the successful event carries Succeeded status
        let ok_usage = usage_events
            .iter()
            .find(|e| e.payload["tool_call_id"] == "tc-gov-ok")
            .expect("ok usage event");
        assert_eq!(ok_usage.payload["status"], "Succeeded");
    }

    #[test]
    fn path_level_write_protection_detects_overlapping_paths() {
        let root = unique_temp_dir("magi-tool-path-conflict");
        let shared_file = root.join("shared.txt");
        fs::write(&shared_file, "data").expect("write shared file");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        // Context A holds a write guard on shared_file via paths
        let ctx_a = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-path-a")),
            session_id: Some(SessionId::new("session-path-a")),
            workspace_id: None,
        };
        let input_a = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-path-a"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf writing",
                "path": shared_file.to_string_lossy(),
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };

        let guard = tool_registry
            .acquire_write_guard(&input_a, &ctx_a, BuiltinToolAccessMode::ExplicitWrite)
            .expect("guard acquisition ok")
            .expect("writeful guard");

        // Context B tries to write to the same path — should conflict
        let ctx_b = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-path-b")),
            session_id: Some(SessionId::new("session-path-b")),
            workspace_id: None,
        };
        let input_b = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tc-path-b"),
            tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "command": "printf conflict",
                "path": shared_file.to_string_lossy(),
                "access_mode": "explicit_write"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };
        let blocked_result =
            tool_registry.acquire_write_guard(&input_b, &ctx_b, BuiltinToolAccessMode::ExplicitWrite);
        assert!(
            blocked_result.is_err(),
            "should be blocked by path-level conflict"
        );
        let err_output = blocked_result.unwrap_err();
        assert_eq!(err_output.status, ExecutionResultStatus::Rejected);
        assert!(err_output.payload.contains("并发写冲突"));

        // After dropping guard A, context B should succeed
        drop(guard);
        let after_result =
            tool_registry.acquire_write_guard(&input_b, &ctx_b, BuiltinToolAccessMode::ExplicitWrite);
        assert!(after_result.is_ok());
        assert!(after_result.unwrap().is_some());
    }

    #[test]
    fn summary_for_query_filters_by_context_fields() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let root = unique_temp_dir("magi-tool-query-filter");
        let file = root.join("q.txt");
        fs::write(&file, "query").expect("write");

        let ctx_w1 = ToolExecutionContext {
            worker_id: Some(WorkerId::new("w1")),
            task_id: Some(TaskId::new("t1")),
            session_id: Some(SessionId::new("s1")),
            workspace_id: Some(WorkspaceId::new("ws1")),
        };
        let ctx_w2 = ToolExecutionContext {
            worker_id: Some(WorkerId::new("w2")),
            task_id: Some(TaskId::new("t2")),
            session_id: Some(SessionId::new("s1")),
            workspace_id: Some(WorkspaceId::new("ws1")),
        };

        // Execute 2 invocations in context w1
        for i in 0..2 {
            tool_registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new(format!("tc-q-w1-{}", i)),
                    tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: file.to_string_lossy().to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                ctx_w1.clone(),
                &ToolExecutionPolicy::default(),
            );
        }
        // Execute 1 invocation in context w2
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-q-w2-0"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx_w2.clone(),
            &ToolExecutionPolicy::default(),
        );

        // Global summary: 3 total
        let all_summary = tool_registry.summary();
        assert_eq!(all_summary.total_invocations, 3);

        // Query by worker_id=w1: 2
        let w1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            worker_id: Some(WorkerId::new("w1")),
            ..Default::default()
        });
        assert_eq!(w1_summary.total_invocations, 2);
        assert_eq!(w1_summary.successful_invocations, 2);

        // Query by worker_id=w2: 1
        let w2_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            worker_id: Some(WorkerId::new("w2")),
            ..Default::default()
        });
        assert_eq!(w2_summary.total_invocations, 1);

        // Query by task_id=t1: 2
        let t1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            task_id: Some(TaskId::new("t1")),
            ..Default::default()
        });
        assert_eq!(t1_summary.total_invocations, 2);

        // Query by session_id=s1: 3 (shared)
        let s1_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            session_id: Some(SessionId::new("s1")),
            ..Default::default()
        });
        assert_eq!(s1_summary.total_invocations, 3);

        // Query by nonexistent worker: 0
        let none_summary = tool_registry.summary_for_query(&ToolExecutionContextQuery {
            worker_id: Some(WorkerId::new("w-nope")),
            ..Default::default()
        });
        assert_eq!(none_summary.total_invocations, 0);
    }

    #[test]
    fn policy_rejection_reflected_in_summary_and_events() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(32));
        let mut tool_registry = ToolRegistry::new(governance, Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        let root = unique_temp_dir("magi-tool-policy-reject");
        let file = root.join("p.txt");
        fs::write(&file, "policy").expect("write");

        let ctx = ToolExecutionContext::default();

        // Policy that explicitly denies file.read
        let deny_policy = ToolExecutionPolicy {
            source_skill_ids: vec!["skill-x".to_string()],
            allowed_tool_names: vec![
                BuiltinToolName::FileRead.as_str().to_string(),
                BuiltinToolName::SearchText.as_str().to_string(),
            ],
            denied_tool_names: vec![BuiltinToolName::FileRead.as_str().to_string()],
        };

        let denied_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-policy-denied"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &deny_policy,
        );
        assert_eq!(denied_output.status, ExecutionResultStatus::Rejected);
        assert_eq!(
            denied_output.governance.outcome,
            GovernanceOutcome::Rejected
        );
        assert_eq!(denied_output.governance.phase, DecisionPhase::ToolPolicy);

        // Policy that only allows search.text — file.read is not in allowed list
        let not_allowed_policy = ToolExecutionPolicy {
            source_skill_ids: vec!["skill-y".to_string()],
            allowed_tool_names: vec![BuiltinToolName::SearchText.as_str().to_string()],
            denied_tool_names: vec![],
        };

        let not_allowed_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-policy-not-allowed"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &not_allowed_policy,
        );
        assert_eq!(not_allowed_output.status, ExecutionResultStatus::Rejected);

        // Now do a successful one with default policy
        let ok_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tc-policy-ok"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(ok_output.status, ExecutionResultStatus::Succeeded);

        // ── Summary: 3 total, 1 success, 2 blocked (policy rejections are Rejected status) ──
        let summary = tool_registry.summary();
        assert_eq!(summary.total_invocations, 3);
        assert_eq!(summary.successful_invocations, 1);
        assert_eq!(summary.blocked_invocations, 2);
        assert_eq!(summary.failed_invocations, 0);

        // ── Events must also carry 3 audit + 3 usage entries ──
        let snapshot = event_bus.snapshot();
        let audit_count = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
            .count();
        assert_eq!(audit_count, 3);

        let usage_count = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
            .count();
        assert_eq!(usage_count, 3);

        // Verify ledger counts match
        let ledger = event_bus.audit_usage_ledger_status();
        assert_eq!(ledger.audit_count, 3);
        assert!(ledger.usage_count >= 3);
    }

    #[test]
    fn full_chain_invocations_events_summary_consistent() {
        // Execute a diverse set of operations and verify every accounting surface agrees.
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(64));
        let mut tool_registry = ToolRegistry::new(Arc::clone(&governance), Arc::clone(&event_bus));
        tool_registry.register_default_builtins();

        let root = unique_temp_dir("magi-tool-full-chain");
        let file = root.join("chain.txt");
        fs::write(&file, "chain data").expect("write");

        let ctx = ToolExecutionContext {
            worker_id: Some(WorkerId::new("wk-chain")),
            task_id: Some(TaskId::new("td-chain")),
            session_id: Some(SessionId::new("ss-chain")),
            workspace_id: Some(WorkspaceId::new("ws-chain")),
        };

        // 1) Successful file read
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-1-ok"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 2) Successful diff preview
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-2-diff"),
                tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({"before": "a", "after": "b"}).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 3) Governance-blocked shell exec (high risk)
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-3-blocked"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "rm -rf /".to_string(),
                approval_requirement: ApprovalRequirement::Required,
                risk_level: RiskLevel::High,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 4) Failed file read (nonexistent)
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-4-fail"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: root.join("nonexistent.txt").to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy::default(),
        );

        // 5) Policy-rejected
        tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("chain-5-policy"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: file.to_string_lossy().to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ctx.clone(),
            &ToolExecutionPolicy {
                source_skill_ids: vec!["sk-locked".to_string()],
                allowed_tool_names: vec![BuiltinToolName::SearchText.as_str().to_string()],
                denied_tool_names: vec![],
            },
        );

        // ── Source 1: invocations list ──
        let invocations = tool_registry.invocations();
        assert_eq!(invocations.len(), 5, "invocations vec has 5 records");

        // ── Source 2: summary ──
        let summary = tool_registry.summary();
        assert_eq!(summary.total_invocations, 5);
        assert_eq!(summary.successful_invocations, 2); // chain-1, chain-2
        assert_eq!(summary.blocked_invocations, 2); // chain-3 (NeedsApproval), chain-5 (Rejected)
        assert_eq!(summary.failed_invocations, 1); // chain-4

        // ── Source 3: event_bus audit events ──
        let snapshot = event_bus.snapshot();
        let audit_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Audit && e.event_type == "tool.invoked")
            .collect();
        assert_eq!(audit_events.len(), 5, "5 audit events");

        // ── Source 4: event_bus usage events ──
        let usage_events: Vec<_> = snapshot
            .recent_events
            .iter()
            .filter(|e| e.category == EventCategory::Usage && e.event_type == "tool.usage.recorded")
            .collect();
        assert_eq!(usage_events.len(), 5, "5 usage events");

        // ── Cross-check: each invocation has matching audit + usage events ──
        for record in &invocations {
            let call_id = record.tool_call_id.to_string();
            let matching_audit = audit_events
                .iter()
                .find(|e| e.payload["tool_call_id"] == call_id);
            assert!(
                matching_audit.is_some(),
                "audit event for {}",
                call_id
            );

            let matching_usage = usage_events
                .iter()
                .find(|e| e.payload["tool_call_id"] == call_id);
            assert!(
                matching_usage.is_some(),
                "usage event for {}",
                call_id
            );

            // Status must agree between invocation record and usage event
            let usage_status = matching_usage.unwrap().payload["status"]
                .as_str()
                .unwrap();
            assert_eq!(
                usage_status,
                format!("{:?}", record.status),
                "status match for {}",
                call_id
            );
        }

        // ── Ledger counts match ──
        let ledger = event_bus.audit_usage_ledger_status();
        assert_eq!(ledger.audit_count, 5);
        assert!(ledger.usage_count >= 5);
    }

    fn make_registry() -> ToolRegistry {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut r = ToolRegistry::new(governance, event_bus);
        r.register_default_builtins();
        r
    }

    fn exec_tool(registry: &ToolRegistry, tool: BuiltinToolName, input: &str) -> ToolExecutionOutput {
        registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new(format!("tc-{}", tool.as_str())),
                tool_name: tool.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: input.to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        )
    }

    #[test]
    fn file_write_creates_and_overwrites() {
        let root = unique_temp_dir("magi-tool-file-write");
        let registry = make_registry();
        let file = root.join("new_file.txt");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "hello world"
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "updated"
            }).to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&file).unwrap(), "updated");
    }

    #[test]
    fn file_write_creates_parent_dirs() {
        let root = unique_temp_dir("magi-tool-file-write-mkdir");
        let registry = make_registry();
        let file = root.join("a").join("b").join("c.txt");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "deep"
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&file).unwrap(), "deep");
    }

    #[test]
    fn file_write_rejects_overwrite_when_disabled() {
        let root = unique_temp_dir("magi-tool-file-write-no-overwrite");
        let registry = make_registry();
        let file = root.join("existing.txt");
        fs::write(&file, "original").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "replaced",
                "overwrite": false
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert_eq!(fs::read_to_string(&file).unwrap(), "original");
    }

    #[test]
    fn file_patch_applies_single_replacement() {
        let root = unique_temp_dir("magi-tool-file-patch");
        let registry = make_registry();
        let file = root.join("patch_me.txt");
        fs::write(&file, "line1\nold_value\nline3").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "old_value",
                "new_string": "new_value"
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 1);
        assert_eq!(fs::read_to_string(&file).unwrap(), "line1\nnew_value\nline3");
    }

    #[test]
    fn file_patch_applies_multiple_patches() {
        let root = unique_temp_dir("magi-tool-file-patch-multi");
        let registry = make_registry();
        let file = root.join("multi.txt");
        fs::write(&file, "aaa\nbbb\nccc").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "patches": [
                    { "old_string": "aaa", "new_string": "AAA" },
                    { "old_string": "ccc", "new_string": "CCC" }
                ]
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 2);
        assert_eq!(fs::read_to_string(&file).unwrap(), "AAA\nbbb\nCCC");
    }

    #[test]
    fn file_patch_rejects_ambiguous_match() {
        let root = unique_temp_dir("magi-tool-file-patch-ambig");
        let registry = make_registry();
        let file = root.join("dup.txt");
        fs::write(&file, "same\nsame\nother").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "same",
                "new_string": "replaced"
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert_eq!(fs::read_to_string(&file).unwrap(), "same\nsame\nother");
    }

    #[test]
    fn file_remove_deletes_file_and_directory() {
        let root = unique_temp_dir("magi-tool-file-remove");
        let registry = make_registry();
        let file = root.join("del_me.txt");
        fs::write(&file, "bye").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": file.to_string_lossy() }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert!(!file.exists());

        let subdir = root.join("nested");
        fs::create_dir_all(subdir.join("child")).unwrap();
        fs::write(subdir.join("child").join("f.txt"), "x").unwrap();

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileRemove,
            &serde_json::json!({ "path": subdir.to_string_lossy(), "recursive": true }).to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        assert!(!subdir.exists());
    }

    #[test]
    fn file_mkdir_creates_nested_dirs() {
        let root = unique_temp_dir("magi-tool-file-mkdir");
        let registry = make_registry();
        let deep = root.join("x").join("y").join("z");

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileMkdir,
            &serde_json::json!({ "path": deep.to_string_lossy() }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert!(deep.is_dir());

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileMkdir,
            &serde_json::json!({ "path": deep.to_string_lossy() }).to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output2.payload).unwrap();
        assert_eq!(payload["already_existed"], true);
    }

    #[test]
    fn file_copy_copies_file_and_directory() {
        let root = unique_temp_dir("magi-tool-file-copy");
        let registry = make_registry();

        let src = root.join("src.txt");
        let dst = root.join("dst.txt");
        fs::write(&src, "copy me").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileCopy,
            &serde_json::json!({
                "source": src.to_string_lossy(),
                "destination": dst.to_string_lossy()
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(fs::read_to_string(&dst).unwrap(), "copy me");
        assert!(src.exists());

        let src_dir = root.join("src_dir");
        fs::create_dir_all(src_dir.join("sub")).unwrap();
        fs::write(src_dir.join("sub").join("f.txt"), "nested").unwrap();
        let dst_dir = root.join("dst_dir");

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileCopy,
            &serde_json::json!({
                "source": src_dir.to_string_lossy(),
                "destination": dst_dir.to_string_lossy()
            }).to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            fs::read_to_string(dst_dir.join("sub").join("f.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn file_move_renames_file() {
        let root = unique_temp_dir("magi-tool-file-move");
        let registry = make_registry();

        let src = root.join("old.txt");
        let dst = root.join("new.txt");
        fs::write(&src, "move me").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileMove,
            &serde_json::json!({
                "source": src.to_string_lossy(),
                "destination": dst.to_string_lossy()
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "move me");
    }

    #[test]
    fn file_move_rejects_existing_destination_without_overwrite() {
        let root = unique_temp_dir("magi-tool-file-move-no-overwrite");
        let registry = make_registry();

        let src = root.join("a.txt");
        let dst = root.join("b.txt");
        fs::write(&src, "from").unwrap();
        fs::write(&dst, "existing").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FileMove,
            &serde_json::json!({
                "source": src.to_string_lossy(),
                "destination": dst.to_string_lossy()
            }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(src.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "existing");
    }

    // ── from_str 映射 + helper 方法 ──

    #[test]
    fn from_str_handles_all_canonical_names() {
        let all_tools = [
            ("file.read", BuiltinToolName::FileRead),
            ("file.write", BuiltinToolName::FileWrite),
            ("file.patch", BuiltinToolName::FilePatch),
            ("file.remove", BuiltinToolName::FileRemove),
            ("file.mkdir", BuiltinToolName::FileMkdir),
            ("file.copy", BuiltinToolName::FileCopy),
            ("file.move", BuiltinToolName::FileMove),
            ("search.text", BuiltinToolName::SearchText),
            ("search.semantic", BuiltinToolName::SearchSemantic),
            ("shell.exec", BuiltinToolName::ShellExec),
            ("process.inspect", BuiltinToolName::ProcessInspect),
            ("diff.preview", BuiltinToolName::DiffPreview),
            ("web.search", BuiltinToolName::WebSearch),
            ("web.fetch", BuiltinToolName::WebFetch),
            ("mermaid.diagram", BuiltinToolName::MermaidDiagram),
            ("knowledge.query", BuiltinToolName::KnowledgeQuery),
            ("orchestration.worker_send_message", BuiltinToolName::WorkerSendMessage),
            ("orchestration.todo_split", BuiltinToolName::TodoSplit),
            ("orchestration.todo_list", BuiltinToolName::TodoList),
            ("orchestration.todo_update", BuiltinToolName::TodoUpdate),
            ("orchestration.todo_claim_next", BuiltinToolName::TodoClaimNext),
            ("orchestration.context_compact", BuiltinToolName::ContextCompact),
            ("skill.apply", BuiltinToolName::SkillApply),
        ];
        for (name, expected) in &all_tools {
            assert_eq!(
                BuiltinToolName::from_str(name),
                Some(*expected),
                "canonical name {} should resolve",
                name
            );
        }
    }

    #[test]
    fn from_str_handles_ts_compatible_aliases() {
        let aliases = [
            ("file_view", BuiltinToolName::FileRead),
            ("file_create", BuiltinToolName::FileWrite),
            ("file_edit", BuiltinToolName::FilePatch),
            ("file_insert", BuiltinToolName::FilePatch),
            ("file_remove", BuiltinToolName::FileRemove),
            ("code_search_regex", BuiltinToolName::SearchText),
            ("code_search_semantic", BuiltinToolName::SearchSemantic),
            ("shell", BuiltinToolName::ShellExec),
            ("web_search", BuiltinToolName::WebSearch),
            ("web_fetch", BuiltinToolName::WebFetch),
            ("mermaid_diagram", BuiltinToolName::MermaidDiagram),
            ("project_knowledge_query", BuiltinToolName::KnowledgeQuery),
            ("worker_send_message", BuiltinToolName::WorkerSendMessage),
            ("todo_split", BuiltinToolName::TodoSplit),
            ("todo_list", BuiltinToolName::TodoList),
            ("todo_update", BuiltinToolName::TodoUpdate),
            ("todo_claim_next", BuiltinToolName::TodoClaimNext),
            ("context_compact", BuiltinToolName::ContextCompact),
            ("skill_apply", BuiltinToolName::SkillApply),
        ];
        for (alias, expected) in &aliases {
            assert_eq!(
                BuiltinToolName::from_str(alias),
                Some(*expected),
                "TS alias {} should resolve",
                alias
            );
        }
        assert_eq!(BuiltinToolName::from_str("nonexistent_tool"), None);
    }

    #[test]
    fn from_str_roundtrips_through_as_str() {
        let all = [
            BuiltinToolName::FileRead, BuiltinToolName::FileWrite, BuiltinToolName::FilePatch,
            BuiltinToolName::FileRemove, BuiltinToolName::FileMkdir, BuiltinToolName::FileCopy,
            BuiltinToolName::FileMove, BuiltinToolName::SearchText, BuiltinToolName::SearchSemantic,
            BuiltinToolName::ShellExec, BuiltinToolName::ProcessInspect, BuiltinToolName::DiffPreview,
            BuiltinToolName::WebSearch, BuiltinToolName::WebFetch, BuiltinToolName::MermaidDiagram,
            BuiltinToolName::KnowledgeQuery, BuiltinToolName::WorkerSendMessage,
            BuiltinToolName::TodoSplit, BuiltinToolName::TodoList, BuiltinToolName::TodoUpdate,
            BuiltinToolName::TodoClaimNext, BuiltinToolName::ContextCompact, BuiltinToolName::SkillApply,
        ];
        for tool in &all {
            assert_eq!(
                BuiltinToolName::from_str(tool.as_str()),
                Some(*tool),
                "{:?} roundtrip failed",
                tool
            );
        }
    }

    #[test]
    fn is_orchestration_identifies_correct_tools() {
        let orchestration = [
            BuiltinToolName::WorkerSendMessage,
            BuiltinToolName::TodoSplit,
            BuiltinToolName::TodoList,
            BuiltinToolName::TodoUpdate,
            BuiltinToolName::TodoClaimNext,
            BuiltinToolName::ContextCompact,
        ];
        let non_orchestration = [
            BuiltinToolName::FileRead, BuiltinToolName::ShellExec,
            BuiltinToolName::WebSearch, BuiltinToolName::SkillApply,
            BuiltinToolName::MermaidDiagram, BuiltinToolName::SearchText,
        ];
        for tool in &orchestration {
            assert!(tool.is_orchestration(), "{:?} should be orchestration", tool);
        }
        for tool in &non_orchestration {
            assert!(!tool.is_orchestration(), "{:?} should not be orchestration", tool);
        }
    }

    #[test]
    fn is_write_operation_identifies_correct_tools() {
        let write_ops = [
            BuiltinToolName::FileWrite, BuiltinToolName::FilePatch,
            BuiltinToolName::FileRemove, BuiltinToolName::FileMkdir,
            BuiltinToolName::FileCopy, BuiltinToolName::FileMove,
        ];
        let non_write = [
            BuiltinToolName::FileRead, BuiltinToolName::SearchText,
            BuiltinToolName::ShellExec, BuiltinToolName::WebSearch,
            BuiltinToolName::DiffPreview, BuiltinToolName::MermaidDiagram,
        ];
        for tool in &write_ops {
            assert!(tool.is_write_operation(), "{:?} should be write", tool);
        }
        for tool in &non_write {
            assert!(!tool.is_write_operation(), "{:?} should not be write", tool);
        }
    }

    // ── mermaid.diagram 验证 ──

    #[test]
    fn mermaid_diagram_recognizes_valid_types() {
        let registry = make_registry();
        let valid_codes = [
            ("graph TD\n  A --> B", "flowchart"),
            ("flowchart LR\n  A --> B", "flowchart"),
            ("sequenceDiagram\n  A->>B: Hello", "sequence"),
            ("classDiagram\n  class A", "class"),
            ("stateDiagram-v2\n  [*] --> S", "state"),
            ("erDiagram\n  A ||--o{ B : has", "er"),
            ("gantt\n  title Plan", "gantt"),
            ("pie\n  title Usage", "pie"),
            ("gitGraph\n  commit", "git"),
            ("mindmap\n  root", "mindmap"),
            ("timeline\n  2024", "timeline"),
        ];
        for (code, expected_type) in &valid_codes {
            let output = exec_tool(
                &registry,
                BuiltinToolName::MermaidDiagram,
                &serde_json::json!({ "code": code }).to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Succeeded, "code: {}", code);
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["diagram_type"], *expected_type, "code: {}", code);
        }
    }

    #[test]
    fn mermaid_diagram_rejects_invalid_type() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::MermaidDiagram,
            &serde_json::json!({ "code": "invalid_diagram\n  A --> B" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
    }

    #[test]
    fn mermaid_diagram_rejects_empty_code() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::MermaidDiagram,
            &serde_json::json!({ "code": "  " }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
    }

    // ── 桩工具行为验证 ──

    #[test]
    fn search_semantic_stub_returns_hint() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::SearchSemantic,
            &serde_json::json!({ "query": "test query" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert!(payload["hint"].as_str().unwrap().contains("CodebaseRetrievalService"));
    }

    #[test]
    fn knowledge_query_stub_returns_hint() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::KnowledgeQuery,
            &serde_json::json!({ "category": "architecture" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert!(payload["hint"].as_str().unwrap().contains("ProjectKnowledgeBase"));
    }

    #[test]
    fn orchestration_stubs_return_error() {
        let registry = make_registry();
        let orchestration_tools = [
            BuiltinToolName::WorkerSendMessage,
            BuiltinToolName::TodoSplit,
            BuiltinToolName::TodoList,
            BuiltinToolName::TodoUpdate,
            BuiltinToolName::TodoClaimNext,
            BuiltinToolName::ContextCompact,
        ];
        for tool in &orchestration_tools {
            let output = exec_tool(&registry, *tool, "{}");
            assert_eq!(output.status, ExecutionResultStatus::Failed, "{:?}", tool);
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert!(
                payload["error"].as_str().unwrap().contains("Orchestrator"),
                "{:?} error should mention Orchestrator",
                tool
            );
        }
    }

    #[test]
    fn skill_apply_stub_returns_hint() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::SkillApply,
            &serde_json::json!({ "skill_name": "auto-review" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert!(payload["hint"].as_str().unwrap().contains("SkillRuntime"));
        assert_eq!(payload["skill_name"], "auto-review");
    }

    // ── web 工具 access mode ──

    #[test]
    fn web_tools_are_read_only() {
        let registry = make_registry();
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::WebSearch.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::WebFetch.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::MermaidDiagram.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
    }

    // ── 23 工具全覆盖注册验证 ──

    #[test]
    fn all_23_tools_are_registered() {
        let registry = make_registry();
        let specs = registry.builtin_specs();
        assert_eq!(specs.len(), 23, "应注册 23 个内置工具");
        let all_tools = [
            BuiltinToolName::FileRead, BuiltinToolName::FileWrite, BuiltinToolName::FilePatch,
            BuiltinToolName::FileRemove, BuiltinToolName::FileMkdir, BuiltinToolName::FileCopy,
            BuiltinToolName::FileMove, BuiltinToolName::SearchText, BuiltinToolName::SearchSemantic,
            BuiltinToolName::ShellExec, BuiltinToolName::ProcessInspect, BuiltinToolName::DiffPreview,
            BuiltinToolName::WebSearch, BuiltinToolName::WebFetch, BuiltinToolName::MermaidDiagram,
            BuiltinToolName::KnowledgeQuery, BuiltinToolName::WorkerSendMessage,
            BuiltinToolName::TodoSplit, BuiltinToolName::TodoList, BuiltinToolName::TodoUpdate,
            BuiltinToolName::TodoClaimNext, BuiltinToolName::ContextCompact, BuiltinToolName::SkillApply,
        ];
        for tool in &all_tools {
            assert!(
                registry.builtin_access_mode(tool.as_str()).is_some(),
                "{:?} should be registered",
                tool
            );
        }
    }

    #[test]
    fn builtin_access_mode_reports_write_tools_correctly() {
        let registry = make_registry();
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileRead.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileWrite.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FilePatch.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileRemove.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileMkdir.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileCopy.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::FileMove.as_str()),
            Some(BuiltinToolAccessMode::ExplicitWrite)
        );
        assert_eq!(
            registry.builtin_access_mode(BuiltinToolName::ShellExec.as_str()),
            Some(BuiltinToolAccessMode::MaybeWrite)
        );
    }
}
