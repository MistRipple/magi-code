use serde::{Deserialize, Serialize};

use crate::ids::{MissionId, TaskId};
use crate::value_objects::UtcMillis;

// ---------------------------------------------------------------------------
// TaskKind
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskKind {
    // Plan nodes (can appear in initial graph)
    Objective,
    Phase,
    WorkPackage,
    Action,
    Validation,
    // Runtime nodes (only created during execution)
    Repair,
    Decision,
}

// ---------------------------------------------------------------------------
// TaskStatus
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Draft,
    Ready,
    Running,
    Blocked,
    AwaitingApproval,
    Verifying,
    Repairing,
    Completed,
    Failed,
    Cancelled,
    Skipped,
}

// ---------------------------------------------------------------------------
// TaskPolicy
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskPolicy {
    pub autonomy_level: String,
    pub approval_mode: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub network_mode: String,
    pub command_mode: String,
    pub retry_limit: u32,
    pub repair_limit: u32,
    pub validation_profile: Option<String>,
    pub checkpoint_mode: String,
    pub background_allowed: bool,
    pub escalation_conditions: Vec<String>,
}

// ---------------------------------------------------------------------------
// TaskVariant (L11 TaskPolymorphism)
// ---------------------------------------------------------------------------

/// Task System v2 L11：任务变体。
///
/// 设计文档 01-architecture.md §2.3 列举了 7 个变体，S7 只完整实现两个：
/// - `LocalAgent`：本进程内启动的子 Conversation，沿用 task_llm_loop 全功能执行（默认值）。
/// - `LocalBash`：异步 shell 任务，跳过 LLM 循环，由 dispatch_execution 直接执行 shell。
///
/// 其余 5 个变体（local_workflow / remote_agent / monitor_mcp / in_process_teammate /
/// dream）在后续 slice 引入；新增枚举值时旧持久化数据自动落到 `LocalAgent`（serde 默认）。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskVariant {
    #[default]
    LocalAgent,
    LocalBash {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
}

impl TaskVariant {
    pub fn is_local_bash(&self) -> bool {
        matches!(self, Self::LocalBash { .. })
    }

    pub fn is_local_agent(&self) -> bool {
        matches!(self, Self::LocalAgent)
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub task_id: TaskId,
    pub mission_id: MissionId,
    pub root_task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub kind: TaskKind,
    pub title: String,
    pub goal: String,
    pub status: TaskStatus,
    pub dependency_ids: Vec<TaskId>,
    pub required_children: Vec<TaskId>,
    pub policy_snapshot: Option<TaskPolicy>,
    /// Executor routing metadata stored as JSON to keep v2 runtime-specific
    /// binding shapes out of magi-core.
    pub executor_binding: Option<serde_json::Value>,
    pub context_refs: Vec<String>,
    pub knowledge_refs: Vec<String>,
    pub workspace_scope: Option<String>,
    pub write_scope: Option<String>,
    pub input_refs: Vec<String>,
    pub output_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub retry_count: u32,
    pub repair_count: u32,
    /// Payload for Decision tasks (design 12), stored as JSON because the
    /// decision lifecycle belongs to the v2 task runner rather than core.
    pub decision_payload: Option<serde_json::Value>,
    /// Task System v2 — L11：执行变体；缺省 LocalAgent，兼容旧持久化数据。
    #[serde(default)]
    pub variant: TaskVariant,
    pub created_at: UtcMillis,
    pub updated_at: UtcMillis,
}

impl Task {
    fn executor_binding_str(&self, key: &str) -> Option<&str> {
        self.executor_binding
            .as_ref()
            .and_then(|binding| binding.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn executor_binding_target_role(&self) -> Option<&str> {
        self.executor_binding_str("target_role")
    }

    pub fn executor_binding_parallelism_group(&self) -> Option<&str> {
        self.executor_binding_str("parallelism_group")
    }

    pub fn executor_binding_exclusive_scope(&self) -> Option<&str> {
        self.executor_binding_str("exclusive_scope")
    }
}

// ---------------------------------------------------------------------------
// TaskProjection (view contract)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskProjection {
    pub root_task: Task,
    pub tasks: Vec<Task>,
    pub current_phase: Option<String>,
    pub running_tasks: Vec<TaskId>,
    pub blocked_tasks: Vec<TaskId>,
    pub pending_decisions: Vec<TaskId>,
    pub workpackage_summaries: Vec<WorkPackageSummary>,
    pub validation_summary: Option<String>,
    pub progress_summary: ProgressSummary,
    pub aggregate_status: TaskStatus,
    pub display_status: String,
    pub execution_mode: String,
    pub runner_status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProgressSummary {
    pub total_tasks: u32,
    pub completed_tasks: u32,
    pub settled_tasks: u32,
    pub failed_tasks: u32,
    pub running_tasks: u32,
    pub blocked_tasks: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkPackageSummary {
    pub task_id: String,
    pub title: String,
    pub aggregate_status: TaskStatus,
    pub display_status: String,
    pub progress_ratio: f32,
    pub recent_evidence: Vec<String>,
    pub recent_issues: Vec<String>,
}
