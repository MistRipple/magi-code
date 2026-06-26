use serde::{Deserialize, Serialize};

use crate::ids::{MissionId, TaskId};
use crate::value_objects::UtcMillis;

/// 任务系统 L11：任务运行变体。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    LocalAgent,
    LocalWorkflow,
    RemoteAgent,
    MonitorMcp,
    InProcessTeammate,
    Dream,
}

/// 任务系统 L11：任务生命周期，固定为 5 态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

pub const TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT: &str = "任务运行失败，详情已记录在日志中。";

pub fn task_output_ref_is_internal_runtime_failure(output_ref: &str) -> bool {
    let normalized = output_ref.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }

    [
        "llm invocation failed",
        "model invocation failed",
        "model bridge client",
        "provider transport failed",
        "provider rejected request",
        "invalid base_url",
        "connection refused",
        "connection reset",
        "http client error",
        "spawn_blocking panicked",
        "dispatch spawn_blocking panicked",
        "dispatch failed",
        "thread panicked",
        "panicked at",
        "stack backtrace",
        "backtrace:",
        "internal assembly",
        "dispatcher missing",
        "task_store",
        "runner_manager",
        "workerruntime",
        "humancheckpointstore",
        "sessionstore",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
        || (contains_any(&normalized, &["timed out", "timeout"])
            && contains_any(
                &normalized,
                &["llm", "model", "provider", "transport", "request"],
            ))
}

pub fn public_task_output_refs(status: TaskStatus, output_refs: &[String]) -> Vec<String> {
    if status != TaskStatus::Failed {
        return output_refs.to_vec();
    }

    let mut redacted = false;
    let visible_refs = output_refs
        .iter()
        .filter_map(|output_ref| {
            if task_output_ref_is_internal_runtime_failure(output_ref) {
                redacted = true;
                None
            } else {
                Some(output_ref.clone())
            }
        })
        .collect::<Vec<_>>();

    if redacted && visible_refs.is_empty() {
        vec![TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT.to_string()]
    } else {
        visible_refs
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

/// 任务系统：任务复杂度分层。
///
/// 简单任务不进入 Task，因此这里仅表达进入任务系统后的两类路径：
/// - `ExecutionChain`：中等任务，启用任务链与可选 coordinator；
/// - `LongMission`：复杂任务，额外启用 Mission/Plan/Validation/Checkpoint/HumanCheckpoint。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTier {
    ExecutionChain,
    LongMission,
}

fn default_task_tier() -> TaskTier {
    TaskTier::ExecutionChain
}
// --- AccessProfile

/// 产品级访问模式。它是用户可理解的主权限心智，运行期只从这个枚举读取权限。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessProfile {
    /// 只读分析：允许读、搜索、诊断；写入和外部副作用直接拒绝，不升级访问模式。
    ReadOnly,
    /// 受限执行：默认模式；常规 workspace 操作自动执行，高风险动作直接拦截。
    #[default]
    Restricted,
    /// 完全授权：跳过常规风险拦截；产品级硬阻断和任务/角色约束仍然生效。
    FullAccess,
}

impl AccessProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Restricted => "restricted",
            Self::FullAccess => "full_access",
        }
    }
}

impl std::str::FromStr for AccessProfile {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "read_only" => Ok(Self::ReadOnly),
            "restricted" => Ok(Self::Restricted),
            "full_access" => Ok(Self::FullAccess),
            _ => Err(()),
        }
    }
}
// --- TaskPolicy

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskPolicy {
    pub autonomy_level: String,
    #[serde(default)]
    pub access_profile: AccessProfile,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub denied_paths: Vec<String>,
    pub network_mode: String,
    pub command_mode: String,
    pub retry_limit: u32,
    pub validation_profile: Option<String>,
    pub checkpoint_mode: String,
    #[serde(default = "default_task_tier")]
    pub task_tier: TaskTier,
    pub background_allowed: bool,
    pub escalation_conditions: Vec<String>,
}

/// 任务系统 L11：变体负载。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRuntimePayload {
    #[default]
    None,
}
// --- Task

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
    /// Executor routing metadata stored as JSON to keep runtime-specific binding shapes
    /// out of magi-core.
    pub executor_binding: Option<serde_json::Value>,
    pub knowledge_refs: Vec<String>,
    pub workspace_scope: Option<String>,
    pub write_scope: Option<String>,
    pub input_refs: Vec<String>,
    pub output_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub retry_count: u32,
    /// 任务系统 — L11：运行变体的专用负载。
    #[serde(default)]
    pub runtime_payload: TaskRuntimePayload,
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
// --- TaskProjection (view contract)

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskProjection {
    pub root_task: Task,
    pub tasks: Vec<Task>,
    pub running_tasks: Vec<TaskId>,
    pub pending_tasks: Vec<TaskId>,
    pub completed_tasks: Vec<TaskId>,
    pub failed_tasks: Vec<TaskId>,
    pub killed_tasks: Vec<TaskId>,
    pub progress_summary: ProgressSummary,
    pub aggregate_status: TaskStatus,
    pub display_status: String,
    pub execution_mode: String,
    pub runner_status: String,
    #[serde(default)]
    pub has_recoverable_chain: bool,
    #[serde(default)]
    pub recoverable_branch_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProgressSummary {
    pub total_tasks: u32,
    pub pending_tasks: u32,
    pub running_tasks: u32,
    pub completed_tasks: u32,
    pub failed_tasks: u32,
    pub killed_tasks: u32,
    pub settled_tasks: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_task_output_refs_redacts_internal_failed_details() {
        let output_refs = vec![
            "LLM invocation failed (round 0): provider transport failed: timed out".to_string(),
        ];

        assert_eq!(
            public_task_output_refs(TaskStatus::Failed, &output_refs),
            vec![TASK_RUNTIME_FAILURE_PUBLIC_OUTPUT.to_string()]
        );
    }

    #[test]
    fn public_task_output_refs_preserves_user_meaningful_failure() {
        let output_refs = vec!["测试失败：断言不匹配".to_string()];

        assert_eq!(
            public_task_output_refs(TaskStatus::Failed, &output_refs),
            output_refs
        );
        assert!(!task_output_ref_is_internal_runtime_failure(
            "工具执行失败，任务不能标记完成：file_write: denied"
        ));
    }

    #[test]
    fn public_task_output_refs_preserves_completed_outputs() {
        let output_refs = vec!["provider transport failed: timed out".to_string()];

        assert_eq!(
            public_task_output_refs(TaskStatus::Completed, &output_refs),
            output_refs
        );
    }
}
