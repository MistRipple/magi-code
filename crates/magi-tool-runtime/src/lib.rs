use magi_core::{
    ApprovalRequirement, EventId, ExecutionResultStatus, RiskLevel, SessionId, TaskId, ToolCallId,
    UtcMillis, WorkerId, WorkspaceId,
};
use magi_event_bus::{EventCategory, EventContext, EventEnvelope, InMemoryEventBus};
use magi_governance::{
    DecisionPhase, GovernanceDecision, GovernanceOutcome, GovernanceService, ToolExecutionRequest,
    ToolKind,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, RwLock},
};

mod builtin;
mod policy;
use builtin::{NormalizedBuiltinTool, infer_execution_status};
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
    ProcessLaunch,
    ProcessRead,
    ProcessWrite,
    ProcessKill,
    ProcessList,
    ProcessInspect,
    // ── Diff ──
    DiffPreview,
    // ── Web ──
    WebSearch,
    WebFetch,
    // ── 可视化 ──
    DiagramRender,
    // ── 知识库 ──
    KnowledgeQuery,
    // ── 协调器（Task System v2 L10 / L11，仅 coordinator_mode 角色可见）──
    /// 派发新的子任务给一个具体角色。返回新建任务的 task_id；子任务完成后，
    /// 其结果会通过 `SendMessage` 路由回父任务的 Mailbox。
    AgentSpawn,
    /// 在同一 mission 内向另一任务投递一条结构化消息（用于父子代理回执、
    /// 子任务之间的协调指令等）。
    SendMessage,
    /// 终止指定任务及其所有 SpawnGraph 后代（级联停止）。
    TaskStop,
    // ── In-session 思维锚点（Task System v2 L13）──
    /// 写入本 session 的 TodoLedger。整体替换列表语义（参考 claude-code 的 TodoWrite）。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    TodoWrite,
    // ── 跨 session 项目记忆（Task System v2 L14）──
    /// 写入或删除当前 workspace 的 ProjectMemory entry。物理存储在
    /// `~/.magi/projects/{slug}/memory/`，跨 conversation 自动加载到 system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    MemoryWrite,
    // ── Mission 宪章（Task System v2 Tier 4 / L15）──
    /// 增量写入当前 mission 的 charter（title / goal / success_criteria /
    /// constraints / stakeholders）。物理存储在
    /// `~/.magi/projects/{slug}/missions/{mission_id}/charter.md`。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    MissionCharterWrite,
    // ── Mission 执行计划（Task System v2 Tier 4 / L16）──
    /// 整体替换当前 mission 的 plan.steps（id / content / status / depends_on / notes）。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/plan.md`，
    /// 每次 Turn 起始把当前 plan 自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    PlanWrite,
    // ── Mission KnowledgeGraph（Task System v2 Tier 4 / L18）──
    /// 按 (kind, id) upsert 当前 mission 的 KnowledgeGraph 事实（symbol / decision / risk）。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/knowledge.md`，
    /// 每次 Turn 起始把当前 KG 自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    KgWrite,
    // ── Mission ValidationRunner（Task System v2 Tier 4 / L19）──
    /// 把单条验证结果（test_suite / type_check / integration_smoke / benchmark）按
    /// (plan_step_id, kind) upsert 进当前 mission 的 ValidationReport。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/validation.md`，
    /// 每次 Turn 起始把当前 Validation 现状自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    ValidationRecord,
    // ── Mission Checkpoint（Task System v2 Tier 4 / L20）──
    /// Append-only 写入一条 mission 级检查点（process_restart / context_compaction /
    /// phase_transition / manual），用于事后恢复 mission 状态。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/checkpoints.md`，
    /// 每次 Turn 起始把最新若干检查点自动注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    Checkpoint,
    // ── Mission HumanCheckpoint（Task System v2 Tier 4 / L21）──
    /// 由 orchestrator 申请的人工审核点，在 operator 给出 approve / reject 之前
    /// mission 会进入 awaiting_human 状态。
    /// 物理存储在 `~/.magi/projects/{slug}/missions/{mission_id}/human_checkpoints.md`，
    /// 每次 Turn 起始把当前待解决与最近若干已解决记录注入 orchestrator system prompt。
    /// 由 orchestration 层拦截，不进入 ToolRegistry。
    HumanCheckpointRequest,
}

impl BuiltinToolName {
    pub const ALL: [Self; 32] = [
        Self::FileRead,
        Self::FileWrite,
        Self::FilePatch,
        Self::FileRemove,
        Self::FileMkdir,
        Self::FileCopy,
        Self::FileMove,
        Self::SearchText,
        Self::SearchSemantic,
        Self::ShellExec,
        Self::ProcessLaunch,
        Self::ProcessRead,
        Self::ProcessWrite,
        Self::ProcessKill,
        Self::ProcessList,
        Self::ProcessInspect,
        Self::DiffPreview,
        Self::WebSearch,
        Self::WebFetch,
        Self::DiagramRender,
        Self::KnowledgeQuery,
        Self::AgentSpawn,
        Self::SendMessage,
        Self::TaskStop,
        Self::TodoWrite,
        Self::MemoryWrite,
        Self::MissionCharterWrite,
        Self::PlanWrite,
        Self::KgWrite,
        Self::ValidationRecord,
        Self::Checkpoint,
        Self::HumanCheckpointRequest,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FileRead => "file_read",
            Self::FileWrite => "file_write",
            Self::FilePatch => "file_patch",
            Self::FileRemove => "file_remove",
            Self::FileMkdir => "file_mkdir",
            Self::FileCopy => "file_copy",
            Self::FileMove => "file_move",
            Self::SearchText => "search_text",
            Self::SearchSemantic => "search_semantic",
            Self::ShellExec => "shell_exec",
            Self::ProcessLaunch => "process_launch",
            Self::ProcessRead => "process_read",
            Self::ProcessWrite => "process_write",
            Self::ProcessKill => "process_kill",
            Self::ProcessList => "process_list",
            Self::ProcessInspect => "process_inspect",
            Self::DiffPreview => "diff_preview",
            Self::WebSearch => "web_search",
            Self::WebFetch => "web_fetch",
            Self::DiagramRender => "diagram_render",
            Self::KnowledgeQuery => "knowledge_query",
            Self::AgentSpawn => "agent_spawn",
            Self::SendMessage => "send_message",
            Self::TaskStop => "task_stop",
            Self::TodoWrite => "todo_write",
            Self::MemoryWrite => "memory_write",
            Self::MissionCharterWrite => "mission_charter_write",
            Self::PlanWrite => "plan_write",
            Self::KgWrite => "kg_write",
            Self::ValidationRecord => "validation_record",
            Self::Checkpoint => "checkpoint_create",
            Self::HumanCheckpointRequest => "human_checkpoint_request",
        }
    }

    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "file_read" | "file_view" => Some(Self::FileRead),
            "file_write" | "file_create" => Some(Self::FileWrite),
            "file_patch" | "file_edit" | "file_insert" => Some(Self::FilePatch),
            "file_remove" => Some(Self::FileRemove),
            "file_mkdir" => Some(Self::FileMkdir),
            "file_copy" => Some(Self::FileCopy),
            "file_move" => Some(Self::FileMove),
            "search_text" | "code_search_regex" => Some(Self::SearchText),
            "search_semantic" | "code_search_semantic" => Some(Self::SearchSemantic),
            "shell_exec" | "shell" => Some(Self::ShellExec),
            "process_launch" => Some(Self::ProcessLaunch),
            "process_read" => Some(Self::ProcessRead),
            "process_write" => Some(Self::ProcessWrite),
            "process_kill" => Some(Self::ProcessKill),
            "process_list" => Some(Self::ProcessList),
            "process_inspect" => Some(Self::ProcessInspect),
            "diff_preview" => Some(Self::DiffPreview),
            "web_search" => Some(Self::WebSearch),
            "web_fetch" => Some(Self::WebFetch),
            "diagram_render" => Some(Self::DiagramRender),
            "knowledge_query" | "project_knowledge_query" => Some(Self::KnowledgeQuery),
            "agent_spawn" | "agent" | "spawn_agent" => Some(Self::AgentSpawn),
            "send_message" | "message_task" => Some(Self::SendMessage),
            "task_stop" | "stop_task" | "cancel_task" => Some(Self::TaskStop),
            "todo_write" | "todowrite" | "todo" => Some(Self::TodoWrite),
            "memory_write" | "memorywrite" | "memory" => Some(Self::MemoryWrite),
            "mission_charter_write" | "missioncharterwrite" | "mission_charter" => {
                Some(Self::MissionCharterWrite)
            }
            "plan_write" | "planwrite" | "plan" => Some(Self::PlanWrite),
            "kg_write" | "kgwrite" | "knowledge_write" | "knowledge_graph_write" => {
                Some(Self::KgWrite)
            }
            "validation_record" | "validationrecord" | "validation_write" | "validation" => {
                Some(Self::ValidationRecord)
            }
            "checkpoint_create" | "checkpoint" | "snapshot" => Some(Self::Checkpoint),
            "human_checkpoint_request" | "human_checkpoint" | "human_review" => {
                Some(Self::HumanCheckpointRequest)
            }
            _ => None,
        }
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

    fn captures_workspace_changes(&self) -> bool {
        self.is_write_operation() || matches!(self, Self::ShellExec)
    }

    pub fn is_public_tool_surface(&self) -> bool {
        !matches!(
            self,
            Self::ProcessLaunch
                | Self::ProcessRead
                | Self::ProcessWrite
                | Self::ProcessKill
                | Self::ProcessList
        )
    }

    pub fn default_risk_level(&self) -> RiskLevel {
        match self {
            Self::FileRead
            | Self::FileMkdir
            | Self::SearchText
            | Self::SearchSemantic
            | Self::ProcessRead
            | Self::ProcessList
            | Self::DiffPreview
            | Self::WebSearch
            | Self::WebFetch
            | Self::DiagramRender
            | Self::KnowledgeQuery
            | Self::SendMessage
            | Self::TodoWrite
            | Self::MemoryWrite
            | Self::MissionCharterWrite
            | Self::PlanWrite
            | Self::KgWrite
            | Self::ValidationRecord
            | Self::Checkpoint
            | Self::HumanCheckpointRequest => RiskLevel::Low,
            Self::FileWrite
            | Self::FilePatch
            | Self::FileCopy
            | Self::FileMove
            | Self::ProcessWrite
            | Self::AgentSpawn => RiskLevel::Medium,
            Self::FileRemove | Self::ShellExec | Self::ProcessLaunch => RiskLevel::High,
            Self::ProcessKill | Self::ProcessInspect | Self::TaskStop => RiskLevel::Medium,
        }
    }

    pub fn default_approval_requirement(&self) -> ApprovalRequirement {
        match self {
            Self::FileRemove | Self::ShellExec | Self::ProcessLaunch => {
                ApprovalRequirement::Required
            }
            _ => ApprovalRequirement::None,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::FileRead => "Read the contents of a file at a given path",
            Self::FileWrite => "Create or overwrite a file with the given content",
            Self::FilePatch => "Apply targeted text replacements to a file (find-and-replace)",
            Self::FileRemove => "Delete a file or directory",
            Self::FileMkdir => "Create a directory (including parent directories)",
            Self::FileCopy => "Copy a file or directory to a new location",
            Self::FileMove => "Move or rename a file or directory",
            Self::SearchText => "Search for text patterns in files within a directory",
            Self::SearchSemantic => {
                "Semantic code search: find code by natural language description"
            }
            Self::ShellExec => "Execute a shell command and return stdout/stderr",
            Self::ProcessLaunch => "Launch a background process in the current session/workspace",
            Self::ProcessRead => "Read stdout/stderr from a managed background process",
            Self::ProcessWrite => "Write input to a managed background process",
            Self::ProcessKill => "Stop a managed background process",
            Self::ProcessList => "List managed background processes in the current context",
            Self::ProcessInspect => "Inspect running processes by PID or name",
            Self::DiffPreview => "Generate a unified diff between two text inputs",
            Self::WebSearch => "Search the web using DuckDuckGo and return results",
            Self::WebFetch => "Fetch content from a URL and convert HTML to markdown",
            Self::DiagramRender => {
                "Render diagrams from Mermaid, DOT, structured graph nodes/edges, or structured flow nodes/edges"
            }
            Self::KnowledgeQuery => {
                "Query project knowledge base: search README, docs, and code documentation"
            }
            Self::AgentSpawn => {
                "Dispatch a sub-task to a registered agent role (architect / integration-dev / reviewer / etc.). Returns the new task_id; the sub-agent's final result will be delivered back via send_message."
            }
            Self::SendMessage => {
                "Deliver a structured message to another task in the same mission. Used by coordinators to forward results, follow-up directives, or sub-agent replies."
            }
            Self::TaskStop => {
                "Terminate a task and cascade-stop all of its descendants in the SpawnGraph. Use only when the entire sub-tree has clearly deviated from the goal or the user has revoked the work."
            }
            Self::TodoWrite => {
                "Replace the current session's TodoLedger with the given list (claude-code TodoWrite semantics). Use to break a long task into steps and track progress; the ledger snapshot is auto-injected into subsequent Turns. Each call overwrites the entire list."
            }
            Self::MemoryWrite => {
                "Persist or remove a ProjectMemory entry for the current workspace. Memory files live under ~/.magi/projects/<slug>/memory/ and are auto-loaded into the system prompt on every new conversation. Use `action: save` to upsert (overwrites the file with the same file_stem) and `action: delete` to remove an entry. Memory kinds: user / feedback / project / reference."
            }
            Self::MissionCharterWrite => {
                "Incrementally update the current mission's charter (title / goal / success_criteria / constraints / stakeholders). The charter persists in ~/.magi/projects/<slug>/missions/<mission_id>/charter.md and is auto-injected into orchestrator prompts. Provide at least one field; omitted fields stay unchanged."
            }
            Self::PlanWrite => {
                "Replace the current mission's execution plan with a complete list of steps. Each step has: id (stable identifier), content (one-line description), status (pending/in_progress/completed/cancelled), depends_on (optional list of step ids), notes (optional). The plan persists in ~/.magi/projects/<slug>/missions/<mission_id>/plan.md and is auto-injected into orchestrator prompts. Use this to draft, evolve, and track multi-step execution strategy for the mission. Each call overwrites the entire step list — provide all steps you want to keep."
            }
            Self::KgWrite => {
                "Upsert one fact into the mission's KnowledgeGraph. Kinds: 'symbol' (code/module index — what classes/interfaces have been migrated, what they own), 'decision' (architecture or trade-off rationale, e.g. why SQLAlchemy over Tortoise), 'risk' (hazards or watch-outs surfaced during execution). Same (kind, id) overwrites the previous fact and bumps its version; set 'tombstoned': true to retire a fact without losing history. KG persists in ~/.magi/projects/<slug>/missions/<mission_id>/knowledge.md and is auto-injected into orchestrator prompts."
            }
            Self::ValidationRecord => {
                "Record one validation result against a Plan step. Kinds: 'test_suite' (unit / integration test runs), 'type_check' (tsc / mypy / cargo check), 'integration_smoke' (cross-process or end-to-end smoke), 'benchmark' (perf / load). Outcomes: 'pass' / 'fail' / 'skipped'. Use this immediately after running a validation command — Coordinator gates a Plan step's completion on having at least one Pass and no unresolved Fail. Same (plan_step_id, kind) overwrites and bumps version. Validation results persist in ~/.magi/projects/<slug>/missions/<mission_id>/validation.md and are auto-injected into orchestrator prompts."
            }
            Self::Checkpoint => {
                "Append one Checkpoint record for the current mission. Kinds: 'process_restart' (record state right before/after a daemon restart), 'context_compaction' (the conversation just got summarized; preserve a recovery pointer), 'phase_transition' (a major plan phase just completed/started), 'manual' (operator-triggered safety net). Checkpoints are append-only; each entry captures a snapshot of plan_version / kg_fact_count / workspace_commit / open conversations so a future Turn can reason about how to recover. Persisted in ~/.magi/projects/<slug>/missions/<mission_id>/checkpoints.md; the latest few entries are auto-injected into orchestrator prompts."
            }
            Self::HumanCheckpointRequest => {
                "Request a human review checkpoint and pause autonomous progress until the operator approves or rejects it. Use this at high-stakes boundaries: irreversible operations, ambiguous trade-offs, large deletions, production deploys, anything requiring human judgement. Required fields: plan_step_id (the Plan step that triggered the request) and prompt_to_human (the question or decision the operator must resolve). Optional: label (short headline) and context (free-form supplementary info). After requesting, do NOT dispatch new work on this mission; resume only after the request is resolved. Persisted in ~/.magi/projects/<slug>/missions/<mission_id>/human_checkpoints.md; pending + recently resolved entries are auto-injected into orchestrator prompts."
            }
        }
    }

    pub fn parameters_schema(&self) -> serde_json::Value {
        match self {
            Self::FileRead => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file to read" },
                    "max_bytes": { "type": "integer", "description": "Maximum number of bytes to read from a file preview" }
                },
                "required": ["path"]
            }),
            Self::FileWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file to write" },
                    "content": { "type": "string", "description": "Content to write to the file" },
                    "overwrite": { "type": "boolean", "description": "Whether to overwrite existing file (default: true)" },
                    "create_dirs": { "type": "boolean", "description": "Whether to create parent directories (default: true)" }
                },
                "required": ["path", "content"]
            }),
            Self::FilePatch => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file to patch" },
                    "old_string": { "type": "string", "description": "Text to find (must match exactly once)" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "patches": {
                        "type": "array",
                        "description": "Array of patches to apply (alternative to old_string/new_string)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
            Self::FileRemove => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file or directory to delete" },
                    "recursive": { "type": "boolean", "description": "Whether to recursively delete directories (default: false)" }
                },
                "required": ["path"]
            }),
            Self::FileMkdir => serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path of the directory to create" }
                },
                "required": ["path"]
            }),
            Self::FileCopy => serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Absolute path of the source file or directory" },
                    "destination": { "type": "string", "description": "Absolute path of the destination" },
                    "overwrite": { "type": "boolean", "description": "Whether to overwrite if destination exists (default: false)" }
                },
                "required": ["source", "destination"]
            }),
            Self::FileMove => serde_json::json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Absolute path of the source file or directory" },
                    "destination": { "type": "string", "description": "Absolute path of the destination" },
                    "overwrite": { "type": "boolean", "description": "Whether to overwrite if destination exists (default: false)" }
                },
                "required": ["source", "destination"]
            }),
            Self::SearchText => serde_json::json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "Root directory to search in" },
                    "query": { "type": "string", "description": "Text pattern to search for" },
                    "limit": { "type": "integer", "description": "Maximum number of results" },
                    "case_sensitive": { "type": "boolean", "description": "Whether the search is case sensitive" },
                    "include_hidden": { "type": "boolean", "description": "Whether hidden files and directories are included" }
                },
                "required": ["root", "query"]
            }),
            Self::SearchSemantic => serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language description of the code to find" },
                    "root": { "type": "string", "description": "Root directory to search in" },
                    "limit": { "type": "integer", "description": "Maximum number of results (default: 10)" }
                },
                "required": ["query"]
            }),
            Self::ShellExec => serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "cwd": { "type": "string", "description": "Working directory" },
                    "shell": { "type": "string", "description": "Shell binary to use" },
                    "timeout_ms": { "type": "integer", "description": "Execution timeout in milliseconds" },
                    "access_mode": {
                        "type": "string",
                        "description": "Declare whether the command is read_only, maybe_write, or explicit_write. Use read_only for inspections such as ls, cat, grep, git status, git diff, and tests that do not modify files.",
                        "enum": ["read_only", "maybe_write", "explicit_write"]
                    },
                    "background": { "type": "boolean", "description": "Launch in the background instead of waiting for completion" }
                },
                "required": ["command"]
            }),
            Self::ProcessLaunch => serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to launch in the background" },
                    "cwd": { "type": "string", "description": "Working directory" },
                    "shell": { "type": "string", "description": "Shell binary to use" }
                },
                "required": ["command"]
            }),
            Self::ProcessRead => serde_json::json!({
                "type": "object",
                "properties": {
                    "terminal_id": { "type": "integer", "description": "Managed terminal/process ID" },
                    "max_bytes": { "type": "integer", "description": "Maximum number of bytes to preview from stdout/stderr" }
                },
                "required": ["terminal_id"]
            }),
            Self::ProcessWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "terminal_id": { "type": "integer", "description": "Managed terminal/process ID" },
                    "input": { "type": "string", "description": "Text to write to the process stdin" },
                    "content": { "type": "string", "description": "Alias for input" },
                    "text": { "type": "string", "description": "Alias for input" }
                },
                "required": ["terminal_id"]
            }),
            Self::ProcessKill => serde_json::json!({
                "type": "object",
                "properties": {
                    "terminal_id": { "type": "integer", "description": "Managed terminal/process ID" }
                },
                "required": ["terminal_id"]
            }),
            Self::ProcessList => serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            Self::ProcessInspect => serde_json::json!({
                "type": "object",
                "properties": {
                    "pid": { "type": "integer", "description": "Process ID to inspect" },
                    "query": { "type": "string", "description": "Process name or search query" },
                    "name": { "type": "string", "description": "Alias for query" },
                    "pattern": { "type": "string", "description": "Alias for query" },
                    "limit": { "type": "integer", "description": "Maximum number of matches" }
                }
            }),
            Self::DiffPreview => serde_json::json!({
                "type": "object",
                "properties": {
                    "before": { "type": "string", "description": "Original text" },
                    "after": { "type": "string", "description": "Modified text" },
                    "before_path": { "type": "string", "description": "Path to the original file" },
                    "after_path": { "type": "string", "description": "Path to the updated file" },
                    "before_label": { "type": "string", "description": "Label for the original side" },
                    "after_label": { "type": "string", "description": "Label for the updated side" },
                    "left": { "type": "string", "description": "Alias for before" },
                    "right": { "type": "string", "description": "Alias for after" },
                    "left_path": { "type": "string", "description": "Alias for before_path" },
                    "right_path": { "type": "string", "description": "Alias for after_path" },
                    "left_label": { "type": "string", "description": "Alias for before_label" },
                    "right_label": { "type": "string", "description": "Alias for after_label" }
                }
            }),
            Self::WebSearch => serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query keywords" }
                },
                "required": ["query"]
            }),
            Self::WebFetch => serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch content from" },
                    "prompt": { "type": "string", "description": "Optional prompt or extraction hint for the fetched page" }
                },
                "required": ["url"]
            }),
            Self::DiagramRender => serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["mermaid", "dot", "graph", "flow"],
                        "description": "Diagram input kind. Use flow for mind maps, hierarchical structures, steps, and process/node flow diagrams. Use graph for relationship/network diagrams. Use mermaid only for Mermaid-specific syntax such as sequence, state, gantt, pie, class, ER, timeline, quadrant, requirement, C4, sankey, xychart, or block diagrams; do not use Mermaid mindmap. Use dot for DOT syntax."
                    },
                    "source": { "type": "string", "description": "Diagram source for mermaid or dot kinds. Mermaid mindmap is not supported on the product surface; represent mind maps with kind=flow or kind=graph and graph.nodes/edges." },
                    "graph": {
                        "type": "object",
                        "description": "Structured graph payload for graph or flow kinds. For mind maps, put the central topic as the first/root node and connect child topics with explicit edges.",
                        "properties": {
                            "nodes": {
                                "type": "array",
                                "description": "Nodes with id, label, and optional group/data fields",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "Stable node id" },
                                        "label": { "type": "string", "description": "Human-readable node label" },
                                        "group": { "type": "string", "description": "Optional node group" },
                                        "position": {
                                            "type": "object",
                                            "properties": {
                                                "x": { "type": "number" },
                                                "y": { "type": "number" }
                                            }
                                        },
                                        "data": { "type": "object", "description": "Optional renderer-specific node metadata" }
                                    },
                                    "required": ["id"]
                                }
                            },
                            "edges": {
                                "type": "array",
                                "description": "Edges with source, target, and optional label/data fields",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "Optional stable edge id" },
                                        "source": { "type": "string", "description": "Source node id" },
                                        "target": { "type": "string", "description": "Target node id" },
                                        "label": { "type": "string", "description": "Human-readable edge label" },
                                        "data": { "type": "object", "description": "Optional renderer-specific edge metadata" }
                                    },
                                    "required": ["source", "target"]
                                }
                            }
                        },
                        "required": ["nodes", "edges"]
                    },
                    "title": { "type": "string", "description": "Optional diagram title" },
                    "layout": {
                        "type": "string",
                        "enum": ["auto", "dagre", "elk", "tidy-tree", "cose", "force", "fcose", "cose-bilkent", "grid", "circle", "preset"],
                        "description": "Preferred layout. auto lets the renderer pick a sensible layout for the kind."
                    },
                    "interactive": { "type": "boolean", "description": "Whether the renderer should enable pan, zoom, and node interaction when supported" },
                    "theme": { "type": "string", "description": "Diagram theme hint" }
                },
                "required": ["kind"]
            }),
            Self::KnowledgeQuery => serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language query to search project documentation" },
                    "category": { "type": "string", "description": "Knowledge category: all, readme, docs, code (default: all)" }
                },
                "required": ["query"]
            }),
            Self::AgentSpawn => serde_json::json!({
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "Registered agent role id, e.g. architect / integration-dev / reviewer / debugger" },
                    "goal": { "type": "string", "description": "Concrete goal for the sub-task; the role-level system prompt will be combined with this goal" },
                    "task_kind": {
                        "type": "string",
                        "enum": ["work_package", "action", "validation", "repair"],
                        "description": "Task kind for the new sub-task. Defaults to action when omitted."
                    },
                    "context": { "type": "string", "description": "Optional context summary handed to the sub-agent (single string)." },
                    "working_dir": { "type": "string", "description": "Optional absolute working directory; defaults to the parent task's workspace root" },
                    "parallelism_group": { "type": "string", "description": "Optional parallelism group name; sub-agents in the same group are mutually exclusive on the same SpawnGraph branch" }
                },
                "required": ["role", "goal"]
            }),
            Self::SendMessage => serde_json::json!({
                "type": "object",
                "properties": {
                    "target_task_id": { "type": "string", "description": "Recipient task id within the same mission" },
                    "payload": { "type": "string", "description": "Message payload — free-form text or JSON-encoded structured data" },
                    "kind": {
                        "type": "string",
                        "enum": ["reply", "directive", "status", "result"],
                        "description": "Message kind hint for the recipient. Defaults to reply."
                    }
                },
                "required": ["target_task_id", "payload"]
            }),
            Self::TaskStop => serde_json::json!({
                "type": "object",
                "properties": {
                    "target_task_id": { "type": "string", "description": "Task to terminate; all descendants in the SpawnGraph are cascade-stopped" },
                    "reason": { "type": "string", "description": "Short reason for the termination, surfaced in the cancelled task's evidence trail" }
                },
                "required": ["target_task_id"]
            }),
            Self::TodoWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "New full list of todos. Replaces the current ledger in its entirety.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "Imperative description of the step, e.g. 'Run tests'"
                                },
                                "activeForm": {
                                    "type": "string",
                                    "description": "Present-continuous form shown while the step is in_progress, e.g. 'Running tests'"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Step status. At most one item should be in_progress at a time."
                                }
                            },
                            "required": ["content", "activeForm", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
            Self::MemoryWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["save", "delete"],
                        "description": "save: upsert a memory entry; delete: remove an existing entry by file_stem."
                    },
                    "file_stem": {
                        "type": "string",
                        "description": "File name without extension. Only [A-Za-z0-9_-] are allowed. Reserved name MEMORY is rejected."
                    },
                    "name": {
                        "type": "string",
                        "description": "Short human title used in the MEMORY.md index. Required when action=save."
                    },
                    "description": {
                        "type": "string",
                        "description": "One-line hook describing what this memory is about; shown in the index. Required when action=save."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["user", "feedback", "project", "reference"],
                        "description": "Memory category. Required when action=save."
                    },
                    "body": {
                        "type": "string",
                        "description": "Full markdown body of the memory file. Required when action=save."
                    }
                },
                "required": ["action", "file_stem"]
            }),
            Self::MissionCharterWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Short headline describing what the mission delivers."
                    },
                    "goal": {
                        "type": "string",
                        "description": "Full statement of the user's intent and the outcome the mission is committing to."
                    },
                    "success_criteria": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Verifiable bullets defining when the mission counts as done."
                    },
                    "constraints": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Hard constraints (scope / tech / time / policy) that bound the mission."
                    },
                    "stakeholders": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "People or roles whose interests must be honored."
                    }
                }
            }),
            Self::PlanWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "description": "Full ordered list of plan steps. Each call replaces the existing plan.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Stable identifier (e.g. 's1', 'audit-deps'). Used by depends_on."
                                },
                                "content": {
                                    "type": "string",
                                    "description": "One-line description of what this step achieves."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "cancelled"],
                                    "description": "Step status; defaults to 'pending' if omitted."
                                },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Optional list of step ids this step depends on. All ids must exist in steps."
                                },
                                "notes": {
                                    "type": "string",
                                    "description": "Optional rationale or scratch note for this step."
                                }
                            },
                            "required": ["id", "content"]
                        }
                    }
                },
                "required": ["steps"]
            }),
            Self::KgWrite => serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["symbol", "decision", "risk"],
                        "description": "Fact bucket. 'symbol' for code/module facts, 'decision' for ADR-style choices, 'risk' for hazards or constraints to watch."
                    },
                    "id": {
                        "type": "string",
                        "description": "Stable id within the bucket. Re-using the same (kind, id) overwrites the previous fact and bumps its version."
                    },
                    "content": {
                        "type": "string",
                        "description": "Statement of the fact in one to a few sentences."
                    },
                    "reference": {
                        "type": "string",
                        "description": "Optional pointer: file path, URL, ADR id — wherever the fact came from."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional free-form tags to help future search/filter."
                    },
                    "tombstoned": {
                        "type": "boolean",
                        "description": "Set to true to retire this fact. Tombstoned facts stay on disk but are hidden from prompt injection."
                    }
                },
                "required": ["kind", "id", "content"]
            }),
            Self::ValidationRecord => serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_step_id": {
                        "type": "string",
                        "description": "Id of the Plan step this validation covers. Must match a step in the current mission's plan.md."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["test_suite", "type_check", "integration_smoke", "benchmark"],
                        "description": "Validation category: unit/integration tests, static type checking, end-to-end smoke, or performance benchmark."
                    },
                    "outcome": {
                        "type": "string",
                        "enum": ["pass", "fail", "skipped"],
                        "description": "Result of the validation run. A Plan step is only considered complete when it has at least one Pass and no unresolved Fail."
                    },
                    "command": {
                        "type": "string",
                        "description": "Optional command line that produced this outcome (e.g. 'cargo test -p magi-api'). Lets the next reader reproduce."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "Optional short summary of the run (count of passing tests, failing assertion, perf number) — keep it diff-friendly."
                    }
                },
                "required": ["plan_step_id", "kind", "outcome"]
            }),
            Self::Checkpoint => serde_json::json!({
                "type": "object",
                "description": "Append a Mission checkpoint record. Recovery kinds (process_restart / context_compaction / phase_transition) MUST carry a non-empty workspace_commit and every entry in open_conversations MUST point at recovery_ref or execution_chain_ref — incomplete recovery sets are rejected.",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["process_restart", "context_compaction", "phase_transition", "manual"],
                        "description": "Checkpoint category. process_restart = daemon restart boundary; context_compaction = conversation just got summarized; phase_transition = a Plan phase boundary; manual = operator-triggered (only manual may skip the recovery set)."
                    },
                    "label": {
                        "type": "string",
                        "description": "Short human-readable label so a future reader can pick the right checkpoint at a glance."
                    },
                    "plan_version": {
                        "type": "integer",
                        "description": "Optional plan version number captured at this checkpoint (lets recovery diff against the current plan)."
                    },
                    "kg_fact_count": {
                        "type": "integer",
                        "description": "Optional count of mission KG facts at this checkpoint."
                    },
                    "workspace_commit": {
                        "type": "string",
                        "description": "Workspace VCS commit/ref captured at this checkpoint. REQUIRED for recovery kinds; only optional when kind=manual."
                    },
                    "open_conversations": {
                        "type": "array",
                        "description": "List of session recovery pointers. Each entry MUST carry session_id plus either recovery_ref or execution_chain_ref so the runtime can rebuild the active execution chain / mailbox after restart.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "session_id": {
                                    "type": "string",
                                    "description": "Session identifier whose execution chain needs to be recoverable."
                                },
                                "recovery_ref": {
                                    "type": "string",
                                    "description": "Pointer into the session-store recovery sidecar (Conversation/Mailbox snapshot). At least one of recovery_ref or execution_chain_ref must be present."
                                },
                                "execution_chain_ref": {
                                    "type": "string",
                                    "description": "Pointer into the active ExecutionChain log so child results can be re-routed to the parent mailbox after recovery."
                                },
                                "turn_cursor": {
                                    "type": "integer",
                                    "description": "Optional last applied turn cursor — used to detect drift between checkpoint and recovery sidecar."
                                },
                                "pending_mailbox": {
                                    "type": "integer",
                                    "description": "Optional count of mailbox items still pending — helps the operator decide whether resume is safe."
                                }
                            },
                            "required": ["session_id"]
                        }
                    },
                    "notes": {
                        "type": "string",
                        "description": "Optional free-form notes (e.g. why this checkpoint, what to watch out for when restoring)."
                    }
                },
                "required": ["kind"]
            }),
            Self::HumanCheckpointRequest => serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_step_id": {
                        "type": "string",
                        "description": "Identifier of the Plan step that triggered the request (must match an existing plan step id)."
                    },
                    "prompt_to_human": {
                        "type": "string",
                        "description": "The question or decision the operator needs to resolve. Be specific: state options, trade-offs, and what 'approve' versus 'reject' means in context."
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional short headline shown alongside the pending request in the operator dashboard."
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional free-form supplementary context (links, snippets, prior decisions) the operator may need to make a call."
                    }
                },
                "required": ["plan_step_id", "prompt_to_human"]
            }),
        }
    }
}

pub fn is_public_builtin_tool_surface(name: &str) -> bool {
    BuiltinToolName::from_str(name)
        .map(|tool| tool.is_public_tool_surface())
        .unwrap_or(false)
}

pub fn is_internal_builtin_tool_surface(name: &str) -> bool {
    BuiltinToolName::from_str(name)
        .map(|tool| !tool.is_public_tool_surface())
        .unwrap_or(false)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<PathBuf>,
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
    pub session_id: Option<SessionId>,
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
    fn execute(&self, input: &str, context: &ToolExecutionContext) -> String;
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
        for name in BuiltinToolName::ALL {
            self.register_builtin(Arc::new(NormalizedBuiltinTool::new(
                name,
                name.default_risk_level(),
                name.default_approval_requirement(),
            )));
        }
    }

    pub fn builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        let mut specs = Vec::with_capacity(self.builtin_tools.len());
        for name in BuiltinToolName::ALL {
            if let Some(tool) = self.builtin_tools.get(name.as_str()) {
                specs.push(tool.spec());
            }
        }
        let mut custom_tools = self
            .builtin_tools
            .iter()
            .filter(|(name, _)| {
                !BuiltinToolName::ALL
                    .iter()
                    .any(|builtin| builtin.as_str() == name.as_str())
            })
            .collect::<Vec<_>>();
        custom_tools.sort_by(|(left, _), (right, _)| left.cmp(right));
        specs.extend(custom_tools.into_iter().map(|(_, tool)| tool.spec()));
        specs
    }

    pub fn public_builtin_specs(&self) -> Vec<BuiltinToolSpec> {
        self.builtin_specs()
            .into_iter()
            .filter(|spec| is_public_builtin_tool_surface(&spec.name))
            .collect()
    }

    pub fn builtin_access_mode(&self, tool_name: &str) -> Option<BuiltinToolAccessMode> {
        self.builtin_tools
            .get(tool_name)
            .map(|_| match BuiltinToolName::from_str(tool_name) {
                Some(name)
                    if name == BuiltinToolName::ShellExec
                        || name == BuiltinToolName::ProcessLaunch =>
                {
                    BuiltinToolAccessMode::MaybeWrite
                }
                Some(name) if name.is_write_operation() => BuiltinToolAccessMode::ExplicitWrite,
                _ => BuiltinToolAccessMode::ReadOnly,
            })
    }

    /// 根据允许/拒绝列表创建过滤后的工具注册表副本。
    pub fn filtered_clone(&self, allowed: &[String], denied: &[String]) -> Self {
        let mut filtered = self.clone();
        if !allowed.is_empty() {
            filtered
                .builtin_tools
                .retain(|name, _| allowed.contains(name));
        }
        if !denied.is_empty() {
            filtered
                .builtin_tools
                .retain(|name, _| !denied.contains(name));
        }
        filtered
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
        self.execute_with_policy_for_surface(input, context, policy, false)
    }

    pub fn cancel_active_shell_execs(&self, query: &ToolExecutionContextQuery) -> usize {
        builtin::cancel_active_shell_execs(query)
    }

    #[cfg(test)]
    fn execute_internal_builtin_with_policy(
        &self,
        input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
    ) -> ToolExecutionOutput {
        self.execute_with_policy_for_surface(input, context, policy, true)
    }

    fn execute_with_policy_for_surface(
        &self,
        mut input: ToolExecutionInput,
        context: ToolExecutionContext,
        policy: &ToolExecutionPolicy,
        allow_internal_builtin_surface: bool,
    ) -> ToolExecutionOutput {
        if input.tool_kind == ToolKind::Builtin
            && let Some(canonical_name) = BuiltinToolName::from_str(input.tool_name.trim())
        {
            input.tool_name = canonical_name.as_str().to_string();
            if !allow_internal_builtin_surface && !canonical_name.is_public_tool_surface() {
                let output = self.build_internal_builtin_surface_rejection(&input, canonical_name);
                self.record_invocation(&input, &context, &output);
                return output;
            }
        }
        if let Some(output) = self.enforce_execution_policy(&input, policy) {
            self.record_invocation(&input, &context, &output);
            return output;
        }

        let governance = self
            .governance
            .evaluate_tool_request(&ToolExecutionRequest {
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
                    let write_guard = match self.acquire_write_guard(&input, &context, access_mode)
                    {
                        Ok(guard) => guard,
                        Err(output) => {
                            self.record_invocation(&input, &context, &output);
                            return output;
                        }
                    };
                    let before_changes = capture_tool_workspace_snapshot(&input, &context);
                    let payload = tool.execute(&input.input, &context);
                    let payload =
                        append_workspace_changed_paths(payload, before_changes.as_ref(), &context);
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

    fn build_internal_builtin_surface_rejection(
        &self,
        input: &ToolExecutionInput,
        tool_name: BuiltinToolName,
    ) -> ToolExecutionOutput {
        let reason = format!(
            "{} 是 shell_exec 的内部运行时能力，不接受模型、worker 或外部调用直接执行；需要后台终端时请调用 shell_exec(background=true)",
            tool_name.as_str()
        );
        ToolExecutionOutput {
            tool_call_id: input.tool_call_id.clone(),
            status: ExecutionResultStatus::Rejected,
            payload: serde_json::json!({
                "tool": tool_name.as_str(),
                "status": "rejected",
                "error": reason.clone(),
            })
            .to_string(),
            governance: GovernanceDecision {
                outcome: GovernanceOutcome::Rejected,
                allowed: false,
                requires_approval: false,
                phase: DecisionPhase::ToolPolicy,
                threshold: input.risk_level,
                reason: Some(reason),
            },
        }
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

    pub fn query_invocations(
        &self,
        query: &ToolExecutionContextQuery,
    ) -> Vec<ToolInvocationRecord> {
        self.invocations
            .read()
            .expect("tool invocation read lock poisoned")
            .iter()
            .filter(|record| {
                query
                    .worker_id
                    .as_ref()
                    .is_none_or(|id| record.context.worker_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .task_id
                    .as_ref()
                    .is_none_or(|id| record.context.task_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .session_id
                    .as_ref()
                    .is_none_or(|id| record.context.session_id.as_ref() == Some(id))
            })
            .filter(|record| {
                query
                    .workspace_id
                    .as_ref()
                    .is_none_or(|id| record.context.workspace_id.as_ref() == Some(id))
            })
            .cloned()
            .collect()
    }

    pub fn summary_for_query(&self, query: &ToolExecutionContextQuery) -> ToolExecutionSummary {
        let invocations = self.query_invocations(query);
        let invocations = self.summarize_invocations(&invocations);
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceChangeSnapshot {
    root: PathBuf,
    files: BTreeMap<String, WorkspaceFileFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceFileFingerprint {
    status_code: String,
    content_hash: Option<u64>,
    exists: bool,
    is_dir: bool,
}

const FILESYSTEM_SNAPSHOT_MAX_FILES: usize = 5000;
const FILESYSTEM_SNAPSHOT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn capture_tool_workspace_snapshot(
    input: &ToolExecutionInput,
    context: &ToolExecutionContext,
) -> Option<WorkspaceChangeSnapshot> {
    if input.tool_kind != ToolKind::Builtin {
        return None;
    }
    let tool_name = BuiltinToolName::from_str(input.tool_name.trim())?;
    if !tool_name.captures_workspace_changes() {
        return None;
    }
    capture_workspace_change_snapshot(context.working_directory.as_deref()?)
}

fn capture_workspace_change_snapshot(workdir: &Path) -> Option<WorkspaceChangeSnapshot> {
    if let Some(repo_root) = run_git_capture(workdir, &["rev-parse", "--show-toplevel"])
        .map(|root| PathBuf::from(root.trim()))
        .filter(|root| !root.as_os_str().is_empty())
    {
        let status = run_git_capture(
            &repo_root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        let mut files = BTreeMap::new();
        for line in status.lines() {
            let Some((status_code, file_path)) = parse_git_status_path(line) else {
                continue;
            };
            files.insert(
                file_path.clone(),
                fingerprint_workspace_file(&repo_root, &file_path, &status_code),
            );
        }
        return Some(WorkspaceChangeSnapshot {
            root: repo_root,
            files,
        });
    }

    capture_filesystem_change_snapshot(workdir)
}

fn capture_filesystem_change_snapshot(root: &Path) -> Option<WorkspaceChangeSnapshot> {
    let root = root
        .canonicalize()
        .ok()
        .or_else(|| Some(root.to_path_buf()))?;
    let mut files = BTreeMap::new();
    collect_filesystem_fingerprints(&root, &root, &mut files);
    Some(WorkspaceChangeSnapshot { root, files })
}

fn append_workspace_changed_paths(
    payload: String,
    before: Option<&WorkspaceChangeSnapshot>,
    context: &ToolExecutionContext,
) -> String {
    let Some(before) = before else {
        return payload;
    };
    let Some(after) = capture_workspace_change_snapshot(
        context
            .working_directory
            .as_deref()
            .unwrap_or_else(|| before.root.as_path()),
    ) else {
        return payload;
    };
    if after.root != before.root {
        return payload;
    }
    let changed_paths = workspace_changed_paths(before, &after);
    if changed_paths.is_empty() {
        return payload;
    }
    append_changed_paths_to_json_payload(payload, &changed_paths)
}

fn workspace_changed_paths(
    before: &WorkspaceChangeSnapshot,
    after: &WorkspaceChangeSnapshot,
) -> Vec<String> {
    before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.files.get(path) != after.files.get(path))
        .collect()
}

fn append_changed_paths_to_json_payload(payload: String, changed_paths: &[String]) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return payload;
    };
    let Some(object) = value.as_object_mut() else {
        return payload;
    };

    let mut merged = object
        .get("changed_paths")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    merged.extend(changed_paths.iter().cloned());
    object.insert(
        "changed_paths".to_string(),
        serde_json::Value::Array(merged.into_iter().map(serde_json::Value::String).collect()),
    );
    value.to_string()
}

fn run_git_capture(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_git_status_path(line: &str) -> Option<(String, String)> {
    if line.len() < 4 {
        return None;
    }
    let status_code = line.get(..2)?.to_string();
    let path_segment = line.get(3..)?.trim();
    if path_segment.is_empty() {
        return None;
    }
    let file_path = path_segment
        .rsplit(" -> ")
        .next()
        .map(str::trim)
        .filter(|path| !path.is_empty())?
        .to_string();
    Some((status_code, file_path))
}

fn fingerprint_workspace_file(
    repo_root: &Path,
    file_path: &str,
    status_code: &str,
) -> WorkspaceFileFingerprint {
    let absolute_path = repo_root.join(file_path);
    match fs::metadata(&absolute_path) {
        Ok(metadata) => WorkspaceFileFingerprint {
            status_code: status_code.to_string(),
            content_hash: if metadata.is_file() {
                hash_file_contents(&absolute_path)
            } else {
                None
            },
            exists: true,
            is_dir: metadata.is_dir(),
        },
        Err(_) => WorkspaceFileFingerprint {
            status_code: status_code.to_string(),
            content_hash: None,
            exists: false,
            is_dir: false,
        },
    }
}

fn collect_filesystem_fingerprints(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, WorkspaceFileFingerprint>,
) {
    if files.len() >= FILESYSTEM_SNAPSHOT_MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        if files.len() >= FILESYSTEM_SNAPSHOT_MAX_FILES {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if filesystem_snapshot_should_skip_dir(&path) {
                continue;
            }
            collect_filesystem_fingerprints(root, &path, files);
            continue;
        }
        if !metadata.is_file() || metadata.len() > FILESYSTEM_SNAPSHOT_MAX_FILE_BYTES {
            continue;
        }
        let Some(relative_path) = path
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        files.insert(
            relative_path,
            WorkspaceFileFingerprint {
                status_code: "FS".to_string(),
                content_hash: hash_file_contents(&path),
                exists: true,
                is_dir: false,
            },
        );
    }
}

fn filesystem_snapshot_should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "coverage" | ".next" | ".svelte-kit"
    )
}

fn hash_file_contents(path: &Path) -> Option<u64> {
    let bytes = fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
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
        io::{Read, Write},
        net::TcpListener,
        process::Command,
        sync::Arc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

    fn all_builtin_tools() -> [BuiltinToolName; 32] {
        BuiltinToolName::ALL
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
        assert_eq!(payload["tool"], "file_read");
        assert_eq!(payload["access_mode"], "read_only");
        assert_eq!(payload["mode"], "file");
        assert_eq!(payload["truncated"], false);
        assert!(
            payload["content"]
                .as_str()
                .expect("content")
                .contains("hello")
        );

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
        let dir_payload: Value =
            serde_json::from_str(&dir_output.payload).expect("dir payload json");
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
        assert_eq!(usage_payload["tool_name"], "file_read");
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
        assert_eq!(payload["tool"], "search_text");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(
            payload["returned_matches"]
                .as_u64()
                .expect("returned matches")
                >= 2
        );
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
        assert_eq!(payload["tool"], "shell_exec");
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
    fn shell_exec_read_only_git_status_in_non_git_workspace_is_stable_probe() {
        let root = unique_temp_dir("magi-tool-shell-non-git-probe");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-non-git")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-non-git")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-non-git-status"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "git status --short",
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["git_worktree"], false);
        assert_eq!(payload["stdout"], "NOT_GIT_WORKTREE\n");
    }

    #[test]
    fn shell_exec_read_only_compound_git_status_in_non_git_workspace_is_stable_probe() {
        let root = unique_temp_dir("magi-tool-shell-compound-non-git-probe");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-compound-non-git")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-compound-non-git")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };
        let command = format!(
            "pwd && printf '\\n---\\n' && ls -1 {} | head -n 3 && printf '\\n---\\n' && git -C {} status --short",
            root.display(),
            root.display()
        );

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-compound-non-git-status"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": command,
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["exit_code"], 0);
        assert_eq!(payload["git_worktree"], false);
        assert_eq!(payload["stdout"], "NOT_GIT_WORKTREE\n");
    }

    #[test]
    fn shell_exec_records_git_worktree_changed_paths() {
        let root = unique_temp_dir("magi-tool-shell-change-capture");
        Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init should run");
        Command::new("git")
            .args(["config", "user.email", "codex@example.com"])
            .current_dir(&root)
            .output()
            .expect("git email config should run");
        Command::new("git")
            .args(["config", "user.name", "Codex"])
            .current_dir(&root)
            .output()
            .expect("git name config should run");
        fs::write(root.join("tracked-a.txt"), "alpha\n").expect("tracked a should write");
        fs::write(root.join("tracked-b.txt"), "beta\n").expect("tracked b should write");
        Command::new("git")
            .args(["add", "--", "tracked-a.txt", "tracked-b.txt"])
            .current_dir(&root)
            .output()
            .expect("git add should run");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&root)
            .output()
            .expect("git commit should run");

        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-change-capture")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-change-capture")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-change-capture"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf 'alpha changed\\n' > tracked-a.txt && rm tracked-b.txt && mkdir -p tmp && printf 'new file\\n' > tmp/new-a.txt",
                    "access_mode": "explicit_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        let changed_paths = payload["changed_paths"]
            .as_array()
            .expect("changed paths should be recorded")
            .iter()
            .map(|value| value.as_str().expect("path should be string"))
            .collect::<Vec<_>>();
        assert!(changed_paths.contains(&"tracked-a.txt"));
        assert!(changed_paths.contains(&"tracked-b.txt"));
        assert!(changed_paths.contains(&"tmp/new-a.txt"));
    }

    #[test]
    fn shell_exec_records_non_git_filesystem_changed_paths() {
        let root = unique_temp_dir("magi-tool-shell-non-git-change-capture");
        fs::write(root.join("remove-me.txt"), "remove me\n").expect("seed file should write");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            session_id: Some(SessionId::new("session-shell-non-git-change")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-non-git-change")),
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-non-git-change"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf 'new file\\n' > new-a.txt && rm remove-me.txt",
                    "access_mode": "explicit_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        let changed_paths = payload["changed_paths"]
            .as_array()
            .expect("changed paths should be recorded")
            .iter()
            .map(|value| value.as_str().expect("path should be string"))
            .collect::<Vec<_>>();
        assert!(changed_paths.contains(&"new-a.txt"));
        assert!(changed_paths.contains(&"remove-me.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn shell_exec_cancel_active_session_kills_running_command() {
        let registry = make_registry();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-shell-cancel")),
            session_id: Some(SessionId::new("session-shell-cancel")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-cancel")),
            working_directory: None,
        };
        let runner_registry = registry.clone();
        let runner_context = context.clone();
        let runner = std::thread::spawn(move || {
            runner_registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new("tool-call-shell-cancel"),
                    tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: serde_json::json!({
                        "command": "sleep 2",
                        "timeout_ms": 5000
                    })
                    .to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                runner_context,
                &ToolExecutionPolicy::default(),
            )
        });

        std::thread::sleep(Duration::from_millis(100));
        let cancel_started = Instant::now();
        let cancelled = registry.cancel_active_shell_execs(&ToolExecutionContextQuery {
            session_id: context.session_id.clone(),
            workspace_id: context.workspace_id.clone(),
            task_id: None,
            worker_id: None,
        });

        assert_eq!(cancelled, 1);
        let output = runner.join().expect("shell execution thread should join");
        assert!(
            cancel_started.elapsed() < Duration::from_millis(1500),
            "取消 shell_exec 后不应等待 sleep 自然结束"
        );
        assert_eq!(output.status, ExecutionResultStatus::Cancelled);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["cancelled"], true);
    }

    #[test]
    fn shell_exec_rejects_blank_json_command() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-blank"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "command": "   " }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(output.payload.contains("缺少 shell 命令"));
    }

    #[test]
    fn builtin_required_fields_reject_empty_json_objects() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let cases = [
            ("file_read", "缺少文件路径"),
            ("search_text", "缺少搜索关键词"),
            ("shell_exec", "缺少 shell 命令"),
            ("file_remove", "缺少文件路径"),
            ("file_mkdir", "缺少目录路径"),
            ("web_search", "缺少搜索关键词 query"),
            ("web_fetch", "缺少 URL"),
            ("search_semantic", "缺少 query 字段"),
            ("knowledge_query", "缺少 query 字段"),
        ];

        for (tool_name, expected_error) in cases {
            let output = tool_registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new(format!("tool-call-empty-{tool_name}")),
                    tool_name: tool_name.to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: serde_json::json!({}).to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                ToolExecutionContext::default(),
                &ToolExecutionPolicy::default(),
            );

            assert_eq!(
                output.status,
                ExecutionResultStatus::Failed,
                "{tool_name} should reject empty JSON object"
            );
            assert!(
                output.payload.contains(expected_error),
                "{tool_name} payload should contain {expected_error}, got {}",
                output.payload
            );
        }
    }

    #[test]
    fn registry_executes_builtin_aliases_via_canonical_name() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-alias"),
                tool_name: "shell".to_string(),
                tool_kind: ToolKind::Builtin,
                input: "printf alias-ok".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );
        let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["stdout"], "alias-ok");
        let invocations = tool_registry.invocations();
        assert_eq!(invocations[0].tool_name, "shell_exec");
    }

    #[test]
    fn builtins_use_context_working_directory_for_relative_inputs() {
        let root = unique_temp_dir("magi-tool-context-cwd");
        fs::write(root.join("marker.txt"), "workspace-marker").expect("write marker");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            working_directory: Some(root.clone()),
            ..ToolExecutionContext::default()
        };

        let shell_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-context-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "test -f marker.txt && printf workspace-ok",
                    "access_mode": "read_only"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        let shell_payload: Value =
            serde_json::from_str(&shell_output.payload).expect("shell payload should parse");
        assert_eq!(shell_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(shell_payload["stdout"], "workspace-ok");

        let file_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-context-file-read"),
                tool_name: BuiltinToolName::FileRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: "marker.txt".to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        let file_payload: Value =
            serde_json::from_str(&file_output.payload).expect("file payload should parse");
        assert_eq!(file_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(
            file_payload["path"],
            root.join("marker.txt").display().to_string()
        );
        assert_eq!(file_payload["content"], "workspace-marker");

        let search_output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-context-search"),
                tool_name: BuiltinToolName::SearchText.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "query": "workspace-marker" }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );
        let search_payload: Value =
            serde_json::from_str(&search_output.payload).expect("search payload should parse");
        assert_eq!(search_output.status, ExecutionResultStatus::Succeeded);
        assert_eq!(search_payload["root"], root.display().to_string());
        assert_eq!(search_payload["returned_matches"], 1);
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
            working_directory: None,
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
        assert_eq!(blocked_payload["tool"], "shell_exec");
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
    fn shell_exec_isolates_write_guards_by_workspace_and_session() {
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
            working_directory: None,
        };
        let other_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("todo-b")),
            session_id: Some(SessionId::new("session-b")),
            workspace_id: Some(WorkspaceId::new("workspace-b")),
            working_directory: None,
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

        let allowed_other_context = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-workdir-other-context"),
                ..guarded_input.clone()
            },
            other_context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(
            allowed_other_context.status,
            ExecutionResultStatus::Succeeded
        );

        let blocked = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-workdir-blocked"),
                ..guarded_input.clone()
            },
            guarded_context,
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
        assert_eq!(payload["tool"], "process_inspect");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(
            payload["matches"]
                .as_array()
                .expect("matches")
                .iter()
                .any(|item| {
                    item["pid"]
                        .as_u64()
                        .map(|pid| pid as u32 == current_pid)
                        .unwrap_or(false)
                })
        );
    }

    #[test]
    fn process_launch_does_not_block_followup_shell_in_same_session() {
        let root = unique_temp_dir("magi-tool-process-launch");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-launch")),
            session_id: Some(SessionId::new("session-process-launch")),
            workspace_id: Some(WorkspaceId::new("workspace-process-launch")),
            working_directory: None,
        };

        let launch = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 2",
                    "cwd": root.to_string_lossy(),
                    "access_mode": "maybe_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value =
            serde_json::from_str(&launch.payload).expect("launch payload json");
        let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

        let started = Instant::now();
        let followup = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-followup-shell"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf followup",
                    "cwd": root.to_string_lossy(),
                    "access_mode": "maybe_write"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );

        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "后台进程不应阻塞后续 shell"
        );
        assert_eq!(followup.status, ExecutionResultStatus::Succeeded);
        let followup_payload: Value =
            serde_json::from_str(&followup.payload).expect("followup payload json");
        assert_eq!(followup_payload["stdout"], "followup");

        let kill = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-kill"),
                tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn process_launch_rejects_blank_json_command() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-blank"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "command": "   " }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(output.payload.contains("缺少 shell 命令"));
    }

    #[test]
    fn process_tools_reject_missing_session_or_workspace_context() {
        let root = unique_temp_dir("magi-tool-process-context");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch-no-context"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 1",
                    "cwd": root.to_string_lossy()
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(output.payload.contains("需要 session 或 workspace 上下文"));

        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-context")),
            session_id: Some(SessionId::new("session-process-context")),
            workspace_id: Some(WorkspaceId::new("workspace-process-context")),
            working_directory: None,
        };
        let launch = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch-context"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 2",
                    "cwd": root.to_string_lossy()
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value =
            serde_json::from_str(&launch.payload).expect("launch payload json");
        let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

        let read_without_context = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-read-no-context"),
                tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            ToolExecutionContext::default(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(read_without_context.status, ExecutionResultStatus::Failed);
        assert!(
            read_without_context
                .payload
                .contains("需要 session 或 workspace 上下文")
        );

        let kill = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-kill-context"),
                tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
    }

    #[test]
    fn process_tools_do_not_cross_sessions_with_workspace_only_context() {
        let root = unique_temp_dir("magi-tool-process-session-scope");
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let owner_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-owner")),
            session_id: Some(SessionId::new("session-process-owner")),
            workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
            working_directory: None,
        };
        let workspace_only_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-workspace-only")),
            session_id: None,
            workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
            working_directory: None,
        };
        let other_session_context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-process-other")),
            session_id: Some(SessionId::new("session-process-other")),
            workspace_id: Some(WorkspaceId::new("workspace-process-shared")),
            working_directory: None,
        };

        let launch = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-launch-owner"),
                tool_name: BuiltinToolName::ProcessLaunch.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "sleep 2",
                    "cwd": root.to_string_lossy()
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            owner_context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(launch.status, ExecutionResultStatus::Succeeded);
        let launch_payload: Value =
            serde_json::from_str(&launch.payload).expect("launch payload json");
        let terminal_id = launch_payload["terminal_id"].as_u64().expect("terminal id");

        let read_workspace_only = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-read-workspace-only"),
                tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            workspace_only_context.clone(),
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(read_workspace_only.status, ExecutionResultStatus::Failed);
        assert!(
            read_workspace_only
                .payload
                .contains("进程不属于当前 session/workspace")
        );

        let read_other_session = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-read-other-session"),
                tool_name: BuiltinToolName::ProcessRead.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            other_session_context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(read_other_session.status, ExecutionResultStatus::Failed);
        assert!(
            read_other_session
                .payload
                .contains("进程不属于当前 session/workspace")
        );

        let process_list = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-list-workspace-only"),
                tool_name: BuiltinToolName::ProcessList.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({}).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            workspace_only_context,
            &ToolExecutionPolicy::default(),
        );
        let list_payload: Value =
            serde_json::from_str(&process_list.payload).expect("list payload json");
        assert_eq!(process_list.status, ExecutionResultStatus::Succeeded);
        assert!(
            list_payload["processes"]
                .as_array()
                .expect("processes should be array")
                .is_empty()
        );

        let kill = tool_registry.execute_internal_builtin_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-process-kill-owner"),
                tool_name: BuiltinToolName::ProcessKill.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({ "terminal_id": terminal_id }).to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            owner_context,
            &ToolExecutionPolicy::default(),
        );
        assert_eq!(kill.status, ExecutionResultStatus::Succeeded);
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
        assert_eq!(payload["tool"], "diff_preview");
        assert_eq!(payload["access_mode"], "read_only");
        assert!(
            payload["preview"]
                .as_str()
                .expect("preview")
                .contains("+new")
        );
        assert!(
            payload["preview"]
                .as_str()
                .expect("preview")
                .contains("-old")
        );
    }

    #[test]
    fn diff_preview_prefers_inline_text_when_path_labels_are_present() {
        let governance = Arc::new(GovernanceService::default());
        let event_bus = Arc::new(magi_event_bus::InMemoryEventBus::new(16));
        let mut tool_registry = ToolRegistry::new(governance, event_bus);
        tool_registry.register_default_builtins();

        let output = tool_registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-diff-inline-first"),
                tool_name: BuiltinToolName::DiffPreview.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "before": "alpha\nbeta",
                    "after": "alpha\nBETA",
                    "before_path": "before",
                    "after_path": "after"
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
        assert_eq!(payload["tool"], "diff_preview");
        assert!(
            payload["preview"]
                .as_str()
                .expect("preview")
                .contains("+BETA")
        );
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
        assert_eq!(usage_payload["tool_name"], "file_read");
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
            working_directory: None,
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
            working_directory: None,
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
            session_id: Some(SessionId::new("session-path-a")),
            workspace_id: None,
            working_directory: None,
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
        let blocked_result = tool_registry.acquire_write_guard(
            &input_b,
            &ctx_b,
            BuiltinToolAccessMode::ExplicitWrite,
        );
        assert!(
            blocked_result.is_err(),
            "should be blocked by path-level conflict"
        );
        let err_output = blocked_result.unwrap_err();
        assert_eq!(err_output.status, ExecutionResultStatus::Rejected);
        assert!(err_output.payload.contains("并发写冲突"));

        // After dropping guard A, context B should succeed
        drop(guard);
        let after_result = tool_registry.acquire_write_guard(
            &input_b,
            &ctx_b,
            BuiltinToolAccessMode::ExplicitWrite,
        );
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
            working_directory: None,
        };
        let ctx_w2 = ToolExecutionContext {
            worker_id: Some(WorkerId::new("w2")),
            task_id: Some(TaskId::new("t2")),
            session_id: Some(SessionId::new("s1")),
            workspace_id: Some(WorkspaceId::new("ws1")),
            working_directory: None,
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
            working_directory: None,
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
            assert!(matching_audit.is_some(), "audit event for {}", call_id);

            let matching_usage = usage_events
                .iter()
                .find(|e| e.payload["tool_call_id"] == call_id);
            assert!(matching_usage.is_some(), "usage event for {}", call_id);

            // Status must agree between invocation record and usage event
            let usage_status = matching_usage.unwrap().payload["status"].as_str().unwrap();
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

    fn exec_tool(
        registry: &ToolRegistry,
        tool: BuiltinToolName,
        input: &str,
    ) -> ToolExecutionOutput {
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
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], BuiltinToolName::FileWrite.as_str());
        assert_eq!(payload["created"], true);
        assert_eq!(payload["overwritten"], false);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello world");

        let output2 = exec_tool(
            &registry,
            BuiltinToolName::FileWrite,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "updated"
            })
            .to_string(),
        );
        assert_eq!(output2.status, ExecutionResultStatus::Succeeded);
        let payload2: Value = serde_json::from_str(&output2.payload).unwrap();
        assert_eq!(payload2["created"], false);
        assert_eq!(payload2["overwritten"], true);
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
            })
            .to_string(),
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
            })
            .to_string(),
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
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 1);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "line1\nnew_value\nline3"
        );
    }

    #[test]
    fn file_patch_empty_patches_falls_back_to_old_new_fields() {
        let root = unique_temp_dir("magi-tool-file-patch-empty-array");
        let registry = make_registry();
        let file = root.join("patch_me.txt");
        fs::write(&file, "alpha needle beta").unwrap();

        let output = exec_tool(
            &registry,
            BuiltinToolName::FilePatch,
            &serde_json::json!({
                "path": file.to_string_lossy(),
                "old_string": "needle",
                "new_string": "needle_patched",
                "patches": []
            })
            .to_string(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["applied"], 1);
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "alpha needle_patched beta"
        );
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
            })
            .to_string(),
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
            })
            .to_string(),
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
            })
            .to_string(),
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
            })
            .to_string(),
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
            })
            .to_string(),
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
            })
            .to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Failed);
        assert!(src.exists());
        assert_eq!(fs::read_to_string(&dst).unwrap(), "existing");
    }

    // ── from_str 映射 + helper 方法 ──

    #[test]
    fn from_str_handles_all_canonical_names() {
        for tool in all_builtin_tools() {
            assert_eq!(
                BuiltinToolName::from_str(tool.as_str()),
                Some(tool),
                "canonical name {} should resolve",
                tool.as_str()
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
            ("project_knowledge_query", BuiltinToolName::KnowledgeQuery),
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
        assert_eq!(BuiltinToolName::from_str("mermaid_diagram"), None);
    }

    #[test]
    fn from_str_roundtrips_through_as_str() {
        for tool in all_builtin_tools() {
            assert_eq!(
                BuiltinToolName::from_str(tool.as_str()),
                Some(tool),
                "{:?} roundtrip failed",
                tool
            );
        }
    }

    #[test]
    fn is_write_operation_identifies_correct_tools() {
        let write_ops = [
            BuiltinToolName::FileWrite,
            BuiltinToolName::FilePatch,
            BuiltinToolName::FileRemove,
            BuiltinToolName::FileMkdir,
            BuiltinToolName::FileCopy,
            BuiltinToolName::FileMove,
        ];
        let non_write = [
            BuiltinToolName::FileRead,
            BuiltinToolName::SearchText,
            BuiltinToolName::ShellExec,
            BuiltinToolName::WebSearch,
            BuiltinToolName::DiffPreview,
            BuiltinToolName::DiagramRender,
        ];
        for tool in &write_ops {
            assert!(tool.is_write_operation(), "{:?} should be write", tool);
        }
        for tool in &non_write {
            assert!(!tool.is_write_operation(), "{:?} should not be write", tool);
        }
    }

    // ── diagram.render 验证 ──

    #[test]
    fn diagram_render_schema_guides_mind_maps_to_structured_payload() {
        let schema = BuiltinToolName::DiagramRender.parameters_schema();
        let kind_description = schema["properties"]["kind"]["description"]
            .as_str()
            .unwrap_or_default();
        let source_description = schema["properties"]["source"]["description"]
            .as_str()
            .unwrap_or_default();
        let graph_description = schema["properties"]["graph"]["description"]
            .as_str()
            .unwrap_or_default();

        assert!(kind_description.contains("mind maps"));
        assert!(kind_description.contains("do not use Mermaid mindmap"));
        assert!(source_description.contains("Mermaid mindmap is not supported"));
        assert!(graph_description.contains("central topic"));
    }

    #[test]
    fn diagram_render_recognizes_mermaid_types() {
        let registry = make_registry();
        let valid_codes = [
            ("graph TD\n  A --> B", "flowchart"),
            ("flowchart LR\n  A --> B", "flowchart"),
            (
                "---\nconfig:\n  layout: elk\n---\nflowchart LR\n  A --> B",
                "flowchart",
            ),
            ("sequenceDiagram\n  A->>B: Hello", "sequence"),
            ("classDiagram\n  class A", "class"),
            ("stateDiagram-v2\n  [*] --> S", "state"),
            ("erDiagram\n  A ||--o{ B : has", "er"),
            ("gantt\n  title Plan", "gantt"),
            ("pie\n  title Usage", "pie"),
            ("gitGraph\n  commit", "git"),
            ("timeline\n  2024", "timeline"),
        ];
        for (code, expected_type) in &valid_codes {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &serde_json::json!({ "kind": "mermaid", "source": code, "layout": "elk" })
                    .to_string(),
            );
            assert_eq!(
                output.status,
                ExecutionResultStatus::Succeeded,
                "code: {}",
                code
            );
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["tool"], "diagram_render");
            assert_eq!(payload["type"], "diagram_render");
            assert_eq!(payload["kind"], "mermaid");
            assert_eq!(payload["layout"], "elk");
            assert_eq!(payload["diagram_type"], *expected_type, "code: {}", code);
        }
    }

    #[test]
    fn diagram_render_accepts_dot_graph_and_flow_kinds() {
        let registry = make_registry();

        let dot = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "dot",
                "source": "digraph G { A -> B }",
                "title": "DOT"
            })
            .to_string(),
        );
        assert_eq!(dot.status, ExecutionResultStatus::Succeeded);
        let dot_payload: Value = serde_json::from_str(&dot.payload).unwrap();
        assert_eq!(dot_payload["kind"], "dot");
        assert_eq!(dot_payload["diagram_type"], "dot");

        for kind in ["graph", "flow"] {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &serde_json::json!({
                    "kind": kind,
                    "layout": "cose",
                    "graph": {
                        "nodes": [
                            { "id": "a", "label": "A" },
                            { "id": "b", "label": "B" }
                        ],
                        "edges": [
                            { "source": "a", "target": "b", "label": "relates" }
                        ]
                    }
                })
                .to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Succeeded, "{kind}");
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["kind"], kind);
            assert_eq!(payload["layout"], "cose");
            assert_eq!(payload["interactive"], true);
            assert_eq!(payload["graph"]["nodes"].as_array().unwrap().len(), 2);
        }

        for layout in ["fcose", "cose-bilkent"] {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &serde_json::json!({
                    "kind": "graph",
                    "layout": layout,
                    "graph": {
                        "nodes": [
                            { "id": "a", "label": "A" },
                            { "id": "b", "label": "B" }
                        ],
                        "edges": [
                            { "source": "a", "target": "b" }
                        ]
                    }
                })
                .to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Succeeded, "{layout}");
            let payload: Value = serde_json::from_str(&output.payload).unwrap();
            assert_eq!(payload["layout"], layout);
        }
    }

    #[test]
    fn diagram_render_rejects_invalid_inputs() {
        let registry = make_registry();
        for input in [
            serde_json::json!({ "kind": "mermaid", "source": "invalid_diagram\n  A --> B" }),
            serde_json::json!({ "kind": "mermaid", "source": "mindmap\n  root\n    child" }),
            serde_json::json!({ "kind": "mermaid", "source": "  " }),
            serde_json::json!({ "kind": "dot", "source": "A -> B" }),
            serde_json::json!({ "kind": "graph", "graph": { "nodes": [] } }),
            serde_json::json!({ "kind": "cytoscape", "graph": { "nodes": [], "edges": [] } }),
        ] {
            let output = exec_tool(
                &registry,
                BuiltinToolName::DiagramRender,
                &input.to_string(),
            );
            assert_eq!(output.status, ExecutionResultStatus::Failed, "{input}");
        }
    }

    #[test]
    fn diagram_render_requires_structured_payload_for_mind_maps() {
        let registry = make_registry();

        let mermaid_mindmap = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "mermaid",
                "source": "mindmap\n  root((验证自动保存规则))\n    目标\n      确认输出结果"
            })
            .to_string(),
        );
        assert_eq!(mermaid_mindmap.status, ExecutionResultStatus::Failed);
        let failed_payload: Value = serde_json::from_str(&mermaid_mindmap.payload).unwrap();
        assert!(
            failed_payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("kind=flow 或 kind=graph"),
            "{failed_payload}"
        );

        let flow_mindmap = exec_tool(
            &registry,
            BuiltinToolName::DiagramRender,
            &serde_json::json!({
                "kind": "flow",
                "graph": {
                    "nodes": [
                        { "id": "root", "label": "验证自动保存规则" },
                        { "id": "goal", "label": "目标" },
                        { "id": "result", "label": "确认输出结果" }
                    ],
                    "edges": [
                        { "source": "root", "target": "goal" },
                        { "source": "goal", "target": "result" }
                    ]
                }
            })
            .to_string(),
        );
        assert_eq!(flow_mindmap.status, ExecutionResultStatus::Succeeded);
        let flow_payload: Value = serde_json::from_str(&flow_mindmap.payload).unwrap();
        assert_eq!(flow_payload["kind"], "flow");
        assert_eq!(flow_payload["interactive"], true);
    }

    // ── 实际工具行为验证 ──

    #[test]
    fn search_semantic_returns_results_structure() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::SearchSemantic,
            &serde_json::json!({ "query": "test query" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], "search_semantic");
        assert_eq!(payload["status"], "succeeded");
        assert!(payload["results"].is_array());
        assert!(payload["scanned_files"].is_number());
    }

    #[test]
    fn knowledge_query_returns_results_structure() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::KnowledgeQuery,
            &serde_json::json!({ "query": "architecture" }).to_string(),
        );
        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).unwrap();
        assert_eq!(payload["tool"], "knowledge_query");
        assert_eq!(payload["status"], "succeeded");
        assert!(payload["results"].is_array());
        assert!(payload["scanned_docs"].is_number());
    }

    #[test]
    fn skill_apply_is_not_registered_as_builtin() {
        let registry = make_registry();
        assert!(registry.builtin_access_mode("skill_apply").is_none());
        assert!(
            registry
                .builtin_specs()
                .iter()
                .all(|spec| spec.name != "skill_apply")
        );
    }

    #[test]
    fn orchestration_tools_are_not_registered_as_builtins() {
        let registry = make_registry();
        for tool_name in [
            "worker_send_message",
            "task_split",
            "task_list",
            "task_update",
            "task_claim_next",
            "context_compact",
        ] {
            assert!(
                BuiltinToolName::from_str(tool_name).is_none(),
                "{tool_name}"
            );
            assert!(
                registry.builtin_access_mode(tool_name).is_none(),
                "{tool_name}"
            );
            assert!(
                registry
                    .builtin_specs()
                    .iter()
                    .all(|spec| spec.name != tool_name),
                "{tool_name}"
            );
        }
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
            registry.builtin_access_mode(BuiltinToolName::DiagramRender.as_str()),
            Some(BuiltinToolAccessMode::ReadOnly)
        );
    }

    #[test]
    fn web_fetch_reads_local_http_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("local test server address")
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept web_fetch request");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let body = r#"<!doctype html><html><body><main><h1>Smoke Web Fetch</h1><p>alpha beta</p></main></body></html>"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write web_fetch response");
        });

        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::WebFetch,
            &serde_json::json!({ "url": url }).to_string(),
        );
        server.join().expect("local web_fetch server should finish");

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], BuiltinToolName::WebFetch.as_str());
        assert_eq!(payload["status"], "succeeded");
        assert!(
            payload["content"]
                .as_str()
                .expect("content should be string")
                .contains("Smoke Web Fetch")
        );
    }

    #[test]
    #[ignore = "live network smoke for manually verifying DuckDuckGo-backed web_search"]
    fn web_search_live_smoke_returns_json_payload() {
        let registry = make_registry();
        let output = exec_tool(
            &registry,
            BuiltinToolName::WebSearch,
            &serde_json::json!({ "query": "OpenAI" }).to_string(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload json");
        assert_eq!(payload["tool"], BuiltinToolName::WebSearch.as_str());
        assert_eq!(payload["status"], "succeeded");
        assert!(payload["result_count"].is_number());
        assert!(payload["results"].is_array());
    }

    // ── 默认内置工具全覆盖注册验证 ──

    #[test]
    fn all_default_builtins_are_registered() {
        let registry = make_registry();
        let specs = registry.builtin_specs();
        let all_tools = all_builtin_tools();
        assert_eq!(specs.len(), all_tools.len(), "应注册全部默认内置工具");
        for tool in &all_tools {
            assert!(
                registry.builtin_access_mode(tool.as_str()).is_some(),
                "{:?} should be registered",
                tool
            );
        }
    }

    #[test]
    fn public_builtin_specs_exclude_shell_internal_process_tools() {
        let registry = make_registry();
        let public_specs = registry.public_builtin_specs();
        let public_names: Vec<_> = public_specs.iter().map(|spec| spec.name.as_str()).collect();

        assert_eq!(
            public_names,
            vec![
                "file_read",
                "file_write",
                "file_patch",
                "file_remove",
                "file_mkdir",
                "file_copy",
                "file_move",
                "search_text",
                "search_semantic",
                "shell_exec",
                "process_inspect",
                "diff_preview",
                "web_search",
                "web_fetch",
                "diagram_render",
                "knowledge_query",
                "agent_spawn",
                "send_message",
                "task_stop",
                "todo_write",
                "memory_write",
                "mission_charter_write",
                "plan_write",
                "kg_write",
                "validation_record",
                "checkpoint_create",
                "human_checkpoint_request",
            ],
            "public builtin specs must remain the single canonical tool surface"
        );

        assert!(is_public_builtin_tool_surface("shell_exec"));
        assert!(!is_public_builtin_tool_surface("process_launch"));
        assert!(!is_public_builtin_tool_surface("process_read"));
        assert!(!is_public_builtin_tool_surface("process_write"));
        assert!(!is_public_builtin_tool_surface("process_kill"));
        assert!(!is_public_builtin_tool_surface("process_list"));
        assert!(is_public_builtin_tool_surface("process_inspect"));
        assert!(
            public_specs
                .iter()
                .any(|spec| spec.name == BuiltinToolName::ShellExec.as_str())
        );
        for internal_tool in [
            BuiltinToolName::ProcessLaunch,
            BuiltinToolName::ProcessRead,
            BuiltinToolName::ProcessWrite,
            BuiltinToolName::ProcessKill,
            BuiltinToolName::ProcessList,
        ] {
            assert!(
                public_specs
                    .iter()
                    .all(|spec| spec.name != internal_tool.as_str())
            );
        }
    }

    #[test]
    fn diagram_renderer_names_are_not_builtin_tools() {
        for name in [
            "mermaid_diagram",
            "mermaid",
            "graphviz",
            "dot",
            "cytoscape",
            "svelte_flow",
            "svelte-flow",
        ] {
            assert_eq!(
                BuiltinToolName::from_str(name),
                None,
                "{name} must stay a renderer/kind behind diagram_render, not a builtin tool"
            );
            assert!(
                !is_public_builtin_tool_surface(name),
                "{name} must not be accepted as a public builtin surface"
            );
        }
    }

    #[test]
    fn registry_rejects_internal_process_tools_as_public_builtin_calls() {
        let registry = make_registry();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-internal-process")),
            session_id: Some(SessionId::new("session-internal-process")),
            workspace_id: Some(WorkspaceId::new("workspace-internal-process")),
            working_directory: None,
        };

        for (tool_name, input) in [
            (
                BuiltinToolName::ProcessLaunch.as_str(),
                serde_json::json!({ "command": "sleep 1" }),
            ),
            (
                BuiltinToolName::ProcessRead.as_str(),
                serde_json::json!({ "terminal_id": 1 }),
            ),
            (
                BuiltinToolName::ProcessWrite.as_str(),
                serde_json::json!({ "terminal_id": 1, "input": "x" }),
            ),
            (
                BuiltinToolName::ProcessKill.as_str(),
                serde_json::json!({ "terminal_id": 1 }),
            ),
            (BuiltinToolName::ProcessList.as_str(), serde_json::json!({})),
        ] {
            let output = registry.execute_with_policy(
                ToolExecutionInput {
                    tool_call_id: ToolCallId::new(format!("tool-call-{tool_name}-internal")),
                    tool_name: tool_name.to_string(),
                    tool_kind: ToolKind::Builtin,
                    input: input.to_string(),
                    approval_requirement: ApprovalRequirement::None,
                    risk_level: RiskLevel::Low,
                },
                context.clone(),
                &ToolExecutionPolicy::default(),
            );

            assert_eq!(
                output.status,
                ExecutionResultStatus::Rejected,
                "{tool_name} must be rejected before internal process execution"
            );
            assert!(
                output.payload.contains("shell_exec"),
                "{tool_name} rejection should point callers to shell_exec, got {}",
                output.payload
            );
        }
    }

    #[test]
    fn shell_exec_background_keeps_shell_public_payload() {
        let registry = make_registry();
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-shell-background")),
            session_id: Some(SessionId::new("session-shell-background")),
            workspace_id: Some(WorkspaceId::new("workspace-shell-background")),
            working_directory: None,
        };

        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-shell-background"),
                tool_name: BuiltinToolName::ShellExec.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "command": "printf shell-background-ok",
                    "background": true
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Succeeded);
        let payload: Value = serde_json::from_str(&output.payload).expect("payload should parse");
        assert_eq!(payload["tool"], "shell_exec");
        assert_eq!(payload["mode"], "background");
        assert!(payload["terminal_id"].as_u64().is_some());
    }

    #[test]
    fn file_write_execution_respects_active_write_guard() {
        let registry = make_registry();
        let root = unique_temp_dir("magi-tool-file-write-guard");
        let file = root.join("guarded.txt");
        let context = ToolExecutionContext {
            worker_id: None,
            task_id: Some(TaskId::new("task-file-write-guard")),
            session_id: Some(SessionId::new("session-file-write-guard")),
            workspace_id: Some(WorkspaceId::new("workspace-file-write-guard")),
            working_directory: None,
        };
        let held_input = ToolExecutionInput {
            tool_call_id: ToolCallId::new("tool-call-file-write-held"),
            tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
            tool_kind: ToolKind::Builtin,
            input: serde_json::json!({
                "path": file.to_string_lossy(),
                "content": "held"
            })
            .to_string(),
            approval_requirement: ApprovalRequirement::None,
            risk_level: RiskLevel::Low,
        };
        let _held_guard = registry
            .acquire_write_guard(&held_input, &context, BuiltinToolAccessMode::ExplicitWrite)
            .expect("held write guard should acquire");

        let output = registry.execute_with_policy(
            ToolExecutionInput {
                tool_call_id: ToolCallId::new("tool-call-file-write-conflict"),
                tool_name: BuiltinToolName::FileWrite.as_str().to_string(),
                tool_kind: ToolKind::Builtin,
                input: serde_json::json!({
                    "path": file.to_string_lossy(),
                    "content": "conflict"
                })
                .to_string(),
                approval_requirement: ApprovalRequirement::None,
                risk_level: RiskLevel::Low,
            },
            context,
            &ToolExecutionPolicy::default(),
        );

        assert_eq!(output.status, ExecutionResultStatus::Rejected);
        assert!(
            output.payload.contains("并发写冲突"),
            "file_write should be protected by the write guard, got {}",
            output.payload
        );
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
