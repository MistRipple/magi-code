use crate::{AuditUsageLedgerStatus, EventCategory, EventEnvelope};
use magi_core::{
    AssignmentId, MissionId, SessionId, TaskId, UtcMillis, WorkspaceId, public_runtime_summary,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod contract;
#[path = "read_model_aggregates.rs"]
mod read_model_aggregates;
use read_model_aggregates::{
    GovernanceAttentionIds, GovernanceDiagnosticCounts, RuntimeAggregateComponents,
};

pub use contract::{
    RUNTIME_READ_MODEL_CONTRACT_SECTIONS, RUNTIME_READ_MODEL_CONTRACT_VERSION,
    RUNTIME_READ_MODEL_ORDERING_STRATEGY, RUNTIME_READ_MODEL_REQUIRED_VALIDATION_REFS,
    RUNTIME_READ_MODEL_SECTION_ORDERING_RULES, RuntimeContractFreezeClosureSummary,
    RuntimeContractFreezeConsistencySummary, RuntimeContractFreezeEvidenceSummary,
    RuntimeContractFreezeGateSummary, RuntimeContractFreezeReportSummary,
    RuntimeContractFreezeSummary, RuntimeContractValidationSummary, RuntimeSectionOrderingRule,
};
use contract::{runtime_read_model_contract_sections, runtime_read_model_section_ordering_rules};
pub const RUNTIME_LEDGER_SCHEMA_VERSION: &str = "audit-usage-ledger-v1";
pub const RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY: &str = "ledger persistence failed";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EventCategoryCounts {
    pub domain: usize,
    pub audit: usize,
    pub usage: usize,
    pub projection: usize,
    pub system: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeActivitySummary {
    pub execution_group_event_count: usize,
    pub worker_event_count: usize,
    pub tool_event_count: usize,
    pub skill_dispatch_event_count: usize,
    pub executor_event_count: usize,
    pub recovery_event_count: usize,
    pub active_execution_group_ids: Vec<String>,
    pub active_task_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeLedgerSummary {
    pub schema_version: String,
    pub audit_count: usize,
    pub usage_count: usize,
    pub next_sequence: u64,
    pub persistence_path: Option<String>,
    pub last_persist_error: Option<String>,
    pub is_persist_healthy: bool,
    pub last_persisted_at: Option<UtcMillis>,
    pub pending_flush: bool,
    pub readiness: RuntimeLedgerReadinessSummary,
    pub cutover_readiness: RuntimeLedgerReadinessSummary,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeLedgerReadinessSummary {
    pub is_ready: bool,
    pub blocking_issue_count: usize,
    pub blocking_issues: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeMaintenanceSummary {
    pub maintenance_mode: Option<String>,
    pub policy_profile: Option<String>,
    pub mode_reason: Option<String>,
    pub last_tick_at: Option<UtcMillis>,
    pub last_sidecar_outcome: Option<String>,
    pub last_ledger_outcome: Option<String>,
    pub tick_interval_millis: Option<u64>,
    pub sidecar_flush_enabled: bool,
    pub ledger_refresh_enabled: bool,
    pub eager_flush_dirty_sidecars: bool,
    pub refresh_ledger_when_unhealthy: bool,
    pub refresh_ledger_when_never_persisted: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeExecutorSummary {
    pub executor_kind: Option<String>,
    pub executor_id: Option<String>,
    pub executor_version: Option<String>,
    pub executor_instance_id: Option<String>,
    pub executor_lease_id: Option<String>,
    pub request_id: Option<String>,
    pub request_source: Option<String>,
    pub observation_status: Option<String>,
    pub requested_stage: Option<String>,
    pub requested_reuse_policy: Option<String>,
    pub requested_binding_scope: Option<String>,
    pub requested_lease_state: Option<String>,
    pub requested_binding_lifecycle: Option<String>,
    pub requested_process_lifecycle: Option<String>,
    pub requested_process_model: Option<String>,
    pub requested_parallelism: Option<usize>,
    pub requested_step_kinds: Vec<String>,
    pub execution_mode: Option<String>,
    pub protocol_version: Option<String>,
    pub process_model: Option<String>,
    pub lease_state: Option<String>,
    pub binding_lifecycle: Option<String>,
    pub process_lifecycle: Option<String>,
    pub health_status: Option<String>,
    pub health_detail: Option<String>,
    pub reuse_scope: Option<String>,
    pub parallelism_scope: Option<String>,
    pub max_parallelism: Option<usize>,
    pub strict_session_affinity: Option<bool>,
    pub strict_workspace_affinity: Option<bool>,
    pub supported_step_kinds: Vec<String>,
    pub worker_id: Option<String>,
    pub task_id: Option<String>,
    pub failure_layer: Option<String>,
    pub failure_message: Option<String>,
    pub last_observed_at: Option<UtcMillis>,
    pub is_ready: bool,
    pub blocking_issue_count: usize,
    pub blocking_issues: Vec<String>,
    pub is_cutover_candidate: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionGroupRuntimeSummaryEntry {
    pub mission_id: String,
    pub event_count: usize,
    pub audit_event_count: usize,
    pub skill_dispatch_count: usize,
    pub builtin_dispatch_count: usize,
    pub bridge_dispatch_count: usize,
    pub rejected_dispatch_count: usize,
    pub failed_dispatch_count: usize,
    pub active_task_ids: Vec<String>,
    pub context_used_turn_count: usize,
    pub context_used_knowledge_count: usize,
    pub context_used_memory_count: usize,
    pub context_used_shared_item_count: usize,
    pub context_used_file_summary_count: usize,
    pub context_recent_turn_resolved_count: usize,
    pub context_recent_turn_retained_count: usize,
    pub context_recent_turn_session_source_count: usize,
    pub context_recent_turn_project_source_count: usize,
    pub context_recent_turn_provided_source_count: usize,
    pub context_truncation_count: usize,
    pub context_truncation_parts: Vec<String>,
    pub context_knowledge_ids: Vec<String>,
    pub context_knowledge_source_paths: Vec<String>,
    pub context_memory_ids: Vec<String>,
    pub context_memory_extraction_refs: Vec<String>,
    pub context_shared_context_ids: Vec<String>,
    pub context_file_summary_paths: Vec<String>,
    pub context_code_index_knowledge_count: usize,
    pub context_audited_knowledge_count: usize,
    pub context_governed_knowledge_count: usize,
    pub context_extracted_memory_count: usize,
    pub context_provenance_linked_memory_count: usize,
    pub latest_event_type: Option<String>,
    pub current_status: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct MissionProgressSummary {
    total_tasks: usize,
    completed_tasks: usize,
    failed_tasks: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskRuntimeSummaryEntry {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub mission_id: Option<String>,
    pub assignment_id: Option<String>,
    pub event_count: usize,
    pub audit_event_count: usize,
    pub skill_dispatch_count: usize,
    pub builtin_dispatch_count: usize,
    pub bridge_dispatch_count: usize,
    pub rejected_dispatch_count: usize,
    pub failed_dispatch_count: usize,
    pub latest_event_type: Option<String>,
    pub current_status: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AssignmentRuntimeSummaryEntry {
    pub assignment_id: String,
    pub mission_id: Option<String>,
    pub event_count: usize,
    pub audit_event_count: usize,
    pub dispatch_count: usize,
    pub task_ids: Vec<String>,
    pub completed_task_count: usize,
    pub failed_task_count: usize,
    pub latest_event_type: Option<String>,
    pub current_status: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkerRuntimeSummaryEntry {
    pub worker_id: String,
    pub event_count: usize,
    pub audit_event_count: usize,
    pub report_count: usize,
    pub tool_call_count: usize,
    pub skill_dispatch_count: usize,
    pub builtin_dispatch_count: usize,
    pub bridge_dispatch_count: usize,
    pub rejected_dispatch_count: usize,
    pub failed_dispatch_count: usize,
    pub current_task_id: Option<String>,
    pub latest_event_type: Option<String>,
    pub current_status: Option<String>,
    pub current_stage: Option<String>,
    pub executor_kind: Option<String>,
    pub executor_id: Option<String>,
    pub executor_version: Option<String>,
    pub executor_instance_id: Option<String>,
    pub executor_lease_id: Option<String>,
    pub executor_request_id: Option<String>,
    pub executor_request_source: Option<String>,
    pub executor_observation_status: Option<String>,
    pub executor_requested_reuse_policy: Option<String>,
    pub executor_requested_binding_scope: Option<String>,
    pub executor_requested_lease_state: Option<String>,
    pub executor_requested_binding_lifecycle: Option<String>,
    pub executor_requested_process_lifecycle: Option<String>,
    pub executor_requested_process_model: Option<String>,
    pub executor_requested_parallelism: Option<usize>,
    pub executor_requested_step_kinds: Vec<String>,
    pub executor_execution_mode: Option<String>,
    pub executor_process_model: Option<String>,
    pub executor_lease_state: Option<String>,
    pub executor_binding_lifecycle: Option<String>,
    pub executor_process_lifecycle: Option<String>,
    pub executor_reuse_scope: Option<String>,
    pub executor_parallelism_scope: Option<String>,
    pub executor_health_status: Option<String>,
    pub executor_failure_layer: Option<String>,
    pub executor_failure_message: Option<String>,
    pub executor_supported_step_kinds: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolRuntimeSummaryEntry {
    pub tool_name: String,
    pub tool_kind: Option<String>,
    pub event_count: usize,
    pub success_count: usize,
    pub blocked_count: usize,
    pub failed_count: usize,
    pub latest_status: Option<String>,
    pub latest_event_type: Option<String>,
    pub worker_ids: Vec<String>,
    pub task_ids: Vec<String>,
    pub session_ids: Vec<String>,
    pub workspace_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeBranchSummaryEntry {
    pub task_id: String,
    pub worker_id: String,
    pub status: String,
    pub stage: String,
    pub lease_id: Option<String>,
    pub execution_intent_ref: Option<String>,
    pub binding_lifecycle: Option<String>,
    pub checkpoint_stage: Option<String>,
    pub next_step_index: Option<usize>,
    pub checkpoint_at: Option<UtcMillis>,
    pub resume_mode: Option<String>,
    pub is_primary: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeTurnSummaryEntry {
    pub turn_id: String,
    pub turn_seq: u64,
    pub accepted_at: Option<UtcMillis>,
    pub completed_at: Option<UtcMillis>,
    pub response_duration_ms: Option<u64>,
    pub status: String,
    pub user_message: Option<String>,
    pub mission_id: Option<String>,
    pub root_task_id: Option<String>,
    pub execution_chain_ref: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeTurnItemSummaryEntry {
    pub item_id: String,
    pub item_seq: usize,
    pub kind: String,
    pub status: String,
    pub source: String,
    pub title: Option<String>,
    pub content: Option<String>,
    pub task_id: Option<String>,
    pub worker_id: Option<String>,
    pub role_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_status: Option<String>,
    pub tool_arguments: Option<String>,
    pub tool_result: Option<String>,
    pub tool_error: Option<String>,
    pub request_id: Option<String>,
    pub user_message_id: Option<String>,
    pub placeholder_message_id: Option<String>,
    pub timeline_entry_id: Option<String>,
    /// item 归属的 thread_id。前端按此 + thread_registry 的 role_id 判定
    /// 主线 / task 详情归属。
    pub source_thread_id: String,
}

/// 会话最近一次模型请求的上下文窗口观测值。
///
/// 由 `from_events` 从 `model.usage.recorded` 事件提取,仅保留计算上下文预算
/// 所需的原始口径(当前请求的上下文窗口 token 与解析模型名)。事件总线本身不
/// 计算窗口大小,装配 DTO 的 `magi-api` 层会用 `magi-usage-authority` 把该
/// 观测值转换为带窗口与告警级别的 [`SessionRuntimeBudgetEntry`]。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeUsageObservation {
    /// 最近一次成功请求返回的当前上下文窗口占用 token。
    pub context_window_tokens: u64,
    /// 该次请求解析出的模型名,用于推断上下文窗口大小。
    pub resolved_model: Option<String>,
    /// 观测对应的事件时间戳。
    pub observed_at: Option<UtcMillis>,
    /// `estimated` 表示运行中估算，`authoritative` 表示 provider 最终 usage。
    pub measurement: Option<String>,
    /// 观测所处阶段：prefill / streaming / completed。
    pub phase: Option<String>,
    pub turn_id: Option<String>,
    pub call_id: Option<String>,
}

/// 会话上下文预算快照,由 `magi-api` 装配 DTO 时填充。
///
/// 字段口径与前端 `runtimeSnapshot.budgetState` 对齐:`token_used` 为当前
/// 上下文窗口占用,`token_limit` 为解析窗口,`usage_ratio` 为占用率,
/// `warning_level` 为稳定字符串告警级别。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeBudgetEntry {
    pub token_used: u64,
    pub remaining_tokens: u64,
    pub token_limit: u64,
    pub percent_remaining: i64,
    pub usage_ratio: f64,
    pub warning_level: String,
}

/// 会话最近一次上下文压缩记录。
///
/// 这是面向产品可见性的结构化摘要：conversation runtime 完成真实历史替换后
/// 发布 `session.context.compacted`，读模型只保留最近一次，前端据此解释上下文
/// 圆环在自动压缩后的状态变化。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeContextCompactionEntry {
    pub reason: String,
    pub phase: Option<String>,
    pub original_message_count: u64,
    pub compacted_message_count: u64,
    pub original_token_estimate: u64,
    pub compacted_token_estimate: u64,
    pub context_window_tokens: Option<u64>,
    pub token_limit: Option<u64>,
    pub threshold_tokens: Option<u64>,
    pub resolved_model: Option<String>,
    pub compacted_at: Option<UtcMillis>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionRuntimeSummaryEntry {
    pub session_id: String,
    pub event_count: usize,
    pub audit_event_count: usize,
    pub worker_event_count: usize,
    pub tool_event_count: usize,
    pub recovery_event_count: usize,
    pub latest_event_type: Option<String>,
    pub active_execution_group_ids: Vec<String>,
    pub active_task_ids: Vec<String>,
    pub recovery_ids: Vec<String>,
    pub current_status: Option<String>,
    pub last_update: Option<UtcMillis>,
    pub mission_id: Option<String>,
    pub root_task_id: Option<String>,
    pub root_task_status: Option<String>,
    pub root_task_created_at: Option<UtcMillis>,
    pub execution_chain_ref: Option<String>,
    pub recovery_ref: Option<String>,
    pub has_recoverable_chain: bool,
    pub recoverable_branch_count: usize,
    pub active_branches: Vec<SessionRuntimeBranchSummaryEntry>,
    pub current_turn: Option<SessionRuntimeTurnSummaryEntry>,
    pub turn_items: Vec<SessionRuntimeTurnItemSummaryEntry>,
    /// 最近一次模型请求的上下文窗口观测值（由 model.usage.recorded 提取）。
    pub usage_observation: Option<SessionRuntimeUsageObservation>,
    /// 上下文预算快照，由 magi-api 装配 DTO 时用 usage-authority 计算填充。
    pub budget: Option<SessionRuntimeBudgetEntry>,
    /// 最近一次上下文压缩记录。
    pub context_compaction: Option<SessionRuntimeContextCompactionEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceRuntimeSummaryEntry {
    pub workspace_id: String,
    pub event_count: usize,
    pub audit_event_count: usize,
    pub worker_event_count: usize,
    pub tool_event_count: usize,
    pub recovery_event_count: usize,
    pub latest_event_type: Option<String>,
    pub active_execution_group_ids: Vec<String>,
    pub active_task_ids: Vec<String>,
    pub recovery_ids: Vec<String>,
    pub execution_chain_refs: Vec<String>,
    pub current_status: Option<String>,
    pub last_update: Option<UtcMillis>,
    pub execution_chain_ref: Option<String>,
    pub recovery_ref: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DispatchRuntimeSummary {
    pub total_dispatches: usize,
    pub resume_dispatches: usize,
    pub latest_dispatch_reason: Option<String>,
    pub active_assignment_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeDiagnosticSummary {
    pub running_execution_group_count: usize,
    pub failed_execution_group_count: usize,
    pub running_task_count: usize,
    pub failed_task_count: usize,
    pub running_assignment_count: usize,
    pub failed_assignment_count: usize,
    pub active_worker_count: usize,
    pub failed_worker_count: usize,
    pub blocked_tool_count: usize,
    pub failed_tool_count: usize,
    pub governance_total_count: usize,
    pub governance_allowed_count: usize,
    pub governance_needs_approval_count: usize,
    pub governance_blocked_count: usize,
    pub governance_rejected_count: usize,
    pub rejected_skill_dispatch_count: usize,
    pub failed_skill_dispatch_count: usize,
    pub context_execution_group_count: usize,
    pub context_used_knowledge_count: usize,
    pub context_used_memory_count: usize,
    pub context_code_index_knowledge_count: usize,
    pub context_extracted_memory_count: usize,
    pub degraded_executor_count: usize,
    pub unavailable_executor_count: usize,
    pub pending_recovery_count: usize,
    pub resumed_recovery_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeOverviewSummary {
    pub category_counts: EventCategoryCounts,
    pub activity: RuntimeActivitySummary,
    pub diagnostics: RuntimeDiagnosticSummary,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeAttentionSummary {
    pub failed_execution_group_ids: Vec<String>,
    pub failed_task_ids: Vec<String>,
    pub failed_assignment_ids: Vec<String>,
    pub failed_worker_ids: Vec<String>,
    pub blocked_tool_names: Vec<String>,
    pub governance_blocked_task_ids: Vec<String>,
    pub governance_approval_required_task_ids: Vec<String>,
    pub governance_rejected_task_ids: Vec<String>,
    pub governance_blocked_worker_ids: Vec<String>,
    pub governance_approval_required_worker_ids: Vec<String>,
    pub governance_rejected_worker_ids: Vec<String>,
    pub rejected_skill_dispatch_worker_ids: Vec<String>,
    pub failed_skill_dispatch_worker_ids: Vec<String>,
    pub degraded_executor_worker_ids: Vec<String>,
    pub unavailable_executor_worker_ids: Vec<String>,
    pub pending_recovery_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeWorkQueueSummary {
    pub running_execution_group_ids: Vec<String>,
    pub running_task_ids: Vec<String>,
    pub running_assignment_ids: Vec<String>,
    pub active_worker_ids: Vec<String>,
    pub pending_recovery_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecoveryResumeObservationSummary {
    pub total_recoveries: usize,
    pub resume_command_count: usize,
    pub resume_dispatch_count: usize,
    pub mission_resumed_count: usize,
    pub worker_resumed_count: usize,
    pub affected_execution_group_ids: Vec<String>,
    pub affected_worker_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryActivityStage {
    ResumeCommandCreated,
    ResumeDispatchCreated,
    MissionResumed,
    WorkerResumed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryActivityEntry {
    pub recovery_id: String,
    pub stage: RecoveryActivityStage,
    pub event_type: String,
    pub category: EventCategory,
    pub occurred_at: UtcMillis,
    pub sequence: u64,
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub mission_id: Option<MissionId>,
    pub assignment_id: Option<AssignmentId>,
    pub task_id: Option<TaskId>,
    pub worker_id: Option<String>,
    pub execution_chain_ref: Option<String>,
    pub diagnostic_summary: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryDiagnosticSummaryEntry {
    pub recovery_id: String,
    pub event_count: usize,
    pub latest_stage: RecoveryActivityStage,
    pub latest_event_type: String,
    pub latest_sequence: u64,
    pub latest_occurred_at: UtcMillis,
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: Option<SessionId>,
    pub mission_id: Option<MissionId>,
    pub assignment_id: Option<AssignmentId>,
    pub task_id: Option<TaskId>,
    pub worker_id: Option<String>,
    pub execution_chain_ref: Option<String>,
    pub diagnostic_summary: Option<String>,
    pub current_status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecoveryReadModelInput {
    pub active_recovery_ids: Vec<String>,
    pub entries: Vec<RecoveryActivityEntry>,
    pub summaries: Vec<RecoveryDiagnosticSummaryEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeDetailsSummary {
    pub execution_groups: Vec<ExecutionGroupRuntimeSummaryEntry>,
    pub tasks: Vec<TaskRuntimeSummaryEntry>,
    pub assignments: Vec<AssignmentRuntimeSummaryEntry>,
    pub workers: Vec<WorkerRuntimeSummaryEntry>,
    pub tools: Vec<ToolRuntimeSummaryEntry>,
    pub sessions: Vec<SessionRuntimeSummaryEntry>,
    pub workspaces: Vec<WorkspaceRuntimeSummaryEntry>,
    #[serde(default)]
    pub knowledge_audit: Vec<KnowledgeAuditRuntimeEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeAuditRuntimeEntry {
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: UtcMillis,
    pub sequence: u64,
    pub workspace_id: Option<String>,
    pub session_id: Option<String>,
    pub mission_id: Option<String>,
    pub task_id: Option<String>,
    pub consumer: Option<String>,
    pub decision: Option<String>,
    pub status: Option<String>,
    pub failure_reason: Option<String>,
    pub knowledge_ids: Vec<String>,
    pub result_kinds: Vec<String>,
    pub matched_count: usize,
    pub injected_count: usize,
    pub injected_chars: usize,
    pub truncated: bool,
    pub candidate_count: usize,
    pub inserted_count: usize,
}

impl KnowledgeAuditRuntimeEntry {
    fn from_event(event: &EventEnvelope) -> Option<Self> {
        if !matches!(
            event.event_type.as_str(),
            "knowledge.context.selected" | "knowledge.learning.extraction"
        ) {
            return None;
        }
        Some(Self {
            event_id: event.event_id.to_string(),
            event_type: event.event_type.clone(),
            occurred_at: event.occurred_at,
            sequence: event.sequence,
            workspace_id: event.workspace_id.as_ref().map(ToString::to_string),
            session_id: event.session_id.as_ref().map(ToString::to_string),
            mission_id: event.mission_id.as_ref().map(ToString::to_string),
            task_id: event.task_id.as_ref().map(ToString::to_string),
            consumer: event
                .payload
                .get("consumer")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            decision: event
                .payload
                .get("decision")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            status: event
                .payload
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            failure_reason: event
                .payload
                .get("failure_reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            knowledge_ids: nested_string_vec_field(&event.payload, "knowledge_ids"),
            result_kinds: nested_string_vec_field(&event.payload, "result_kinds"),
            matched_count: nested_usize_field(&event.payload, "matched_count"),
            injected_count: nested_usize_field(&event.payload, "injected_count"),
            injected_chars: nested_usize_field(&event.payload, "injected_chars"),
            truncated: event
                .payload
                .get("truncated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            candidate_count: nested_usize_field(&event.payload, "candidate_count"),
            inserted_count: nested_usize_field(&event.payload, "inserted_count"),
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeOperationsSummary {
    pub dispatch: DispatchRuntimeSummary,
    pub attention: RuntimeAttentionSummary,
    pub work_queues: RuntimeWorkQueueSummary,
    pub resume_observation: RecoveryResumeObservationSummary,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeMetaSummary {
    pub contract_version: String,
    pub contract_sections: Vec<String>,
    pub ordering_strategy: String,
    pub section_ordering_rules: Vec<RuntimeSectionOrderingRule>,
    pub ledger: RuntimeLedgerSummary,
    pub maintenance: RuntimeMaintenanceSummary,
    pub executor: RuntimeExecutorSummary,
    pub freeze: RuntimeContractFreezeSummary,
    pub freeze_gate: RuntimeContractFreezeGateSummary,
    pub freeze_evidence: RuntimeContractFreezeEvidenceSummary,
    pub freeze_report: RuntimeContractFreezeReportSummary,
    pub freeze_consistency: RuntimeContractFreezeConsistencySummary,
    pub freeze_closure: RuntimeContractFreezeClosureSummary,
    pub validation: RuntimeContractValidationSummary,
    pub latest_sequence: u64,
    pub recent_event_count: usize,
}

impl From<AuditUsageLedgerStatus> for RuntimeLedgerSummary {
    fn from(value: AuditUsageLedgerStatus) -> Self {
        let last_persist_error = public_ledger_persist_error(value.last_persist_error);
        let is_persist_healthy = last_persist_error.is_none();
        let mut summary = Self {
            schema_version: value.schema_version,
            audit_count: value.audit_count,
            usage_count: value.usage_count,
            next_sequence: value.next_sequence,
            persistence_path: value
                .persistence_path
                .map(|path| path.display().to_string()),
            last_persist_error,
            is_persist_healthy,
            last_persisted_at: None,
            pending_flush: false,
            readiness: RuntimeLedgerReadinessSummary::default(),
            cutover_readiness: RuntimeLedgerReadinessSummary::default(),
        };
        summary.refresh_readiness();
        summary
    }
}

fn public_ledger_persist_error(error: Option<String>) -> Option<String> {
    error.map(|_| RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY.to_string())
}

impl RuntimeLedgerSummary {
    pub fn refresh_readiness(&mut self) {
        let mut blocking_issues = Vec::new();
        if self.persistence_path.is_none() {
            blocking_issues.push("ledger persistence path missing".to_string());
        }
        if !self.is_persist_healthy {
            blocking_issues.push("ledger persistence is unhealthy".to_string());
        }
        self.readiness = RuntimeLedgerReadinessSummary {
            is_ready: blocking_issues.is_empty(),
            blocking_issue_count: blocking_issues.len(),
            blocking_issues,
        };

        let mut cutover_blocking_issues = self.readiness.blocking_issues.clone();
        if self.pending_flush {
            cutover_blocking_issues.push("ledger has pending flush".to_string());
        }
        if self.last_persisted_at.is_none() {
            cutover_blocking_issues.push("ledger has not been persisted yet".to_string());
        }
        self.cutover_readiness = RuntimeLedgerReadinessSummary {
            is_ready: cutover_blocking_issues.is_empty(),
            blocking_issue_count: cutover_blocking_issues.len(),
            blocking_issues: cutover_blocking_issues,
        };
    }
}

impl RuntimeMaintenanceSummary {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        events
            .iter()
            .rev()
            .find(|event| event.event_type == "system.runtime.maintenance.status")
            .map(|event| Self::from_payload(&event.payload))
            .unwrap_or_default()
    }

    fn from_payload(payload: &serde_json::Value) -> Self {
        Self {
            maintenance_mode: payload
                .get("maintenance_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            policy_profile: payload
                .get("policy_profile")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            mode_reason: payload
                .get("mode_reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            last_tick_at: payload
                .get("last_tick_at")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok()),
            last_sidecar_outcome: payload
                .get("last_sidecar_outcome")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            last_ledger_outcome: payload
                .get("last_ledger_outcome")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            tick_interval_millis: payload
                .get("tick_interval_millis")
                .and_then(serde_json::Value::as_u64),
            sidecar_flush_enabled: payload
                .get("sidecar_flush_enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            ledger_refresh_enabled: payload
                .get("ledger_refresh_enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            eager_flush_dirty_sidecars: payload
                .get("eager_flush_dirty_sidecars")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            refresh_ledger_when_unhealthy: payload
                .get("refresh_ledger_when_unhealthy")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            refresh_ledger_when_never_persisted: payload
                .get("refresh_ledger_when_never_persisted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }
    }
}

impl RuntimeExecutorSummary {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        events
            .iter()
            .rev()
            .find(|event| event.event_type == "worker.executor.observed")
            .map(|event| Self::from_payload(&event.payload))
            .unwrap_or_default()
    }

    fn from_payload(payload: &serde_json::Value) -> Self {
        let observation_status = payload
            .get("observation_status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let mut summary = Self {
            executor_kind: payload
                .get("executor_kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            executor_id: payload
                .get("executor_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            executor_version: payload
                .get("executor_version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            executor_instance_id: payload
                .get("executor_instance_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            executor_lease_id: payload
                .get("executor_lease_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            request_id: payload
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            request_source: payload
                .get("request_source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            observation_status: observation_status.clone(),
            requested_stage: payload
                .get("requested_stage")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_reuse_policy: payload
                .get("requested_reuse_policy")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_binding_scope: payload
                .get("requested_binding_scope")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_lease_state: payload
                .get("requested_lease_state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_binding_lifecycle: payload
                .get("requested_binding_lifecycle")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_process_lifecycle: payload
                .get("requested_process_lifecycle")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_process_model: payload
                .get("requested_process_model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            requested_parallelism: payload
                .get("requested_parallelism")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize),
            requested_step_kinds: payload
                .get("requested_step_kinds")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            execution_mode: payload
                .get("execution_mode")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            protocol_version: payload
                .get("protocol_version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            process_model: payload
                .get("process_model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            lease_state: payload
                .get("lease_state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            binding_lifecycle: payload
                .get("binding_lifecycle")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            process_lifecycle: payload
                .get("process_lifecycle")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            health_status: payload
                .get("health_status")
                .and_then(serde_json::Value::as_str)
                .map(|value| value.to_ascii_lowercase()),
            health_detail: payload
                .get("health_detail")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| public_runtime_summary(Some(value))),
            reuse_scope: payload
                .get("reuse_scope")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            parallelism_scope: payload
                .get("parallelism_scope")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            max_parallelism: payload
                .get("max_parallelism")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as usize),
            strict_session_affinity: payload
                .get("strict_session_affinity")
                .and_then(serde_json::Value::as_bool),
            strict_workspace_affinity: payload
                .get("strict_workspace_affinity")
                .and_then(serde_json::Value::as_bool),
            supported_step_kinds: payload
                .get("supported_step_kinds")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            worker_id: payload
                .get("worker_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            task_id: payload
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            failure_layer: payload
                .get("failure_layer")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            failure_message: payload
                .get("failure_message")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| public_runtime_summary(Some(value))),
            last_observed_at: payload
                .get("observed_at")
                .and_then(serde_json::Value::as_u64)
                .map(UtcMillis),
            is_ready: false,
            blocking_issue_count: 0,
            blocking_issues: Vec::new(),
            is_cutover_candidate: false,
        };
        summary.refresh_readiness();
        summary
    }

    pub fn refresh_readiness(&mut self) {
        let mut blocking_issues = Vec::new();
        if !matches!(self.observation_status.as_deref(), Some("ready")) {
            blocking_issues.push("executor observation is not ready".to_string());
        }
        if !matches!(self.health_status.as_deref(), Some("healthy")) {
            blocking_issues.push("executor health is not healthy".to_string());
        }
        if self.failure_layer.is_some() {
            blocking_issues.push("executor reported failure layer".to_string());
        }
        if self.failure_message.is_some() {
            blocking_issues.push("executor reported failure message".to_string());
        }
        if !matches!(self.execution_mode.as_deref(), Some("local-process")) {
            blocking_issues.push("executor is not local-process mode".to_string());
        }
        if self.executor_id.is_none() || self.executor_version.is_none() {
            blocking_issues.push("executor identity is incomplete".to_string());
        }
        if !self
            .supported_step_kinds
            .iter()
            .any(|kind| kind == "final-report")
        {
            blocking_issues.push("executor missing final-report capability".to_string());
        }
        if self.max_parallelism == Some(0) {
            blocking_issues.push("executor max_parallelism is zero".to_string());
        }
        if matches!(self.process_model.as_deref(), Some("persistent-process"))
            && !matches!(
                self.process_lifecycle.as_deref(),
                Some("persistent") | Some("reusable")
            )
        {
            blocking_issues.push(
                "persistent-process executor lifecycle is not reusable/persistent".to_string(),
            );
        }
        if matches!(
            self.reuse_scope.as_deref(),
            Some("session") | Some("workspace")
        ) && !matches!(self.lease_state.as_deref(), Some("active"))
        {
            blocking_issues.push("reusable executor lease is not active".to_string());
        }
        if matches!(
            self.reuse_scope.as_deref(),
            Some("session") | Some("workspace")
        ) && !matches!(self.binding_lifecycle.as_deref(), Some("bound"))
        {
            blocking_issues.push("reusable executor binding is not bound".to_string());
        }
        self.is_ready = blocking_issues.is_empty();
        self.blocking_issue_count = blocking_issues.len();
        self.blocking_issues = blocking_issues;
        self.is_cutover_candidate = self.is_ready
            && matches!(self.process_model.as_deref(), Some("persistent-process"))
            && matches!(self.process_lifecycle.as_deref(), Some("persistent"))
            && self.executor_instance_id.is_some()
            && self.executor_lease_id.is_some()
            && matches!(self.lease_state.as_deref(), Some("active"))
            && matches!(self.binding_lifecycle.as_deref(), Some("bound"))
            && matches!(
                self.reuse_scope.as_deref(),
                Some("session") | Some("workspace")
            );
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeReadModelInput {
    pub meta: RuntimeMetaSummary,
    pub overview: RuntimeOverviewSummary,
    pub details: RuntimeDetailsSummary,
    pub operations: RuntimeOperationsSummary,
    pub recovery: RecoveryReadModelInput,
}

impl RecoveryReadModelInput {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        let mut entries = events
            .iter()
            .filter_map(RecoveryActivityEntry::from_event)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.sequence);

        let mut summary_map = BTreeMap::<String, RecoveryDiagnosticSummaryEntry>::new();
        for entry in &entries {
            let summary = summary_map
                .entry(entry.recovery_id.clone())
                .or_insert_with(|| RecoveryDiagnosticSummaryEntry::from_entry(entry));
            summary.event_count += 1;
            summary.latest_stage = entry.stage;
            summary.latest_event_type = entry.event_type.clone();
            summary.latest_sequence = entry.sequence;
            summary.latest_occurred_at = entry.occurred_at;
            if entry.workspace_id.is_some() {
                summary.workspace_id = entry.workspace_id.clone();
            }
            if entry.session_id.is_some() {
                summary.session_id = entry.session_id.clone();
            }
            if entry.mission_id.is_some() {
                summary.mission_id = entry.mission_id.clone();
            }
            if entry.assignment_id.is_some() {
                summary.assignment_id = entry.assignment_id.clone();
            }
            if entry.task_id.is_some() {
                summary.task_id = entry.task_id.clone();
            }
            if entry.worker_id.is_some() {
                summary.worker_id = entry.worker_id.clone();
            }
            if entry.execution_chain_ref.is_some() {
                summary.execution_chain_ref = entry.execution_chain_ref.clone();
            }
            if entry.diagnostic_summary.is_some() {
                summary.diagnostic_summary = entry.diagnostic_summary.clone();
            }
            summary.current_status = infer_recovery_status(entry.stage);
        }
        let summaries = summary_map.into_values().collect::<Vec<_>>();
        let active_recovery_ids = summaries
            .iter()
            .map(|summary| summary.recovery_id.clone())
            .collect();

        Self {
            active_recovery_ids,
            entries,
            summaries,
        }
        .normalize()
    }

    fn normalize(mut self) -> Self {
        self.active_recovery_ids.sort();
        self.summaries
            .sort_by(|left, right| left.recovery_id.cmp(&right.recovery_id));
        self
    }
}

impl RuntimeReadModelInput {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        let latest_sequence = events.iter().map(|event| event.sequence).max().unwrap_or(0);
        let recent_event_count = events.len();
        let mut category_counts = EventCategoryCounts::default();
        let mut summary = RuntimeActivitySummary::default();
        let mut execution_group_map = BTreeMap::<String, ExecutionGroupRuntimeSummaryEntry>::new();
        let mut mission_progress_map = BTreeMap::<String, MissionProgressSummary>::new();
        let mut task_map = BTreeMap::<String, TaskRuntimeSummaryEntry>::new();
        let mut assignment_map = BTreeMap::<String, AssignmentRuntimeSummaryEntry>::new();
        let mut worker_map = BTreeMap::<String, WorkerRuntimeSummaryEntry>::new();
        let mut tool_map = BTreeMap::<String, ToolRuntimeSummaryEntry>::new();
        let mut session_map = BTreeMap::<String, SessionRuntimeSummaryEntry>::new();
        let mut workspace_map = BTreeMap::<String, WorkspaceRuntimeSummaryEntry>::new();
        let mut knowledge_audit = Vec::new();
        let mut dispatch = DispatchRuntimeSummary::default();
        let mut governance_total_count = 0usize;
        let mut governance_allowed_count = 0usize;
        let mut governance_needs_approval_count = 0usize;
        let mut governance_blocked_count = 0usize;
        let mut governance_rejected_count = 0usize;
        let mut governance_blocked_task_ids = Vec::new();
        let mut governance_approval_required_task_ids = Vec::new();
        let mut governance_rejected_task_ids = Vec::new();
        let mut governance_blocked_worker_ids = Vec::new();
        let mut governance_approval_required_worker_ids = Vec::new();
        let mut governance_rejected_worker_ids = Vec::new();
        for event in events {
            if let Some(entry) = KnowledgeAuditRuntimeEntry::from_event(event) {
                knowledge_audit.push(entry);
            }
            let resolved_mission_id = event_mission_id(event);
            let resolved_task_id = event_task_id(event);
            match event.category {
                EventCategory::Domain => category_counts.domain += 1,
                EventCategory::Audit => category_counts.audit += 1,
                EventCategory::Usage => category_counts.usage += 1,
                EventCategory::Projection => category_counts.projection += 1,
                EventCategory::System => category_counts.system += 1,
            }
            if resolved_mission_id.is_some() {
                summary.execution_group_event_count += 1;
            }
            if event.event_type.starts_with("worker.") {
                summary.worker_event_count += 1;
            }
            if event.event_type == "worker.skill_dispatch.observed" {
                summary.skill_dispatch_event_count += 1;
            }
            if event.event_type == "worker.executor.observed" {
                summary.executor_event_count += 1;
            }
            if event.event_type.starts_with("tool.") {
                summary.tool_event_count += 1;
            }
            if event.event_type.contains(".resume.") || event.event_type.contains(".resumed.") {
                summary.recovery_event_count += 1;
            }
            if let Some(session_id) = event.session_id.as_ref() {
                let session_id = session_id.to_string();
                let session_entry = session_map.entry(session_id.clone()).or_insert_with(|| {
                    SessionRuntimeSummaryEntry {
                        session_id: session_id.clone(),
                        ..SessionRuntimeSummaryEntry::default()
                    }
                });
                session_entry.event_count += 1;
                if event.category == EventCategory::Audit {
                    session_entry.audit_event_count += 1;
                }
                if event.event_type.starts_with("worker.") {
                    session_entry.worker_event_count += 1;
                }
                if event.event_type.starts_with("tool.") {
                    session_entry.tool_event_count += 1;
                }
                if event.event_type.contains(".resume.") || event.event_type.contains(".resumed.") {
                    session_entry.recovery_event_count += 1;
                }
                session_entry.latest_event_type = Some(event.event_type.clone());
                if let Some(observation) = usage_observation_from_event(event) {
                    session_entry.usage_observation = Some(observation);
                }
                if let Some(compaction) = context_compaction_from_event(event) {
                    session_entry.context_compaction = Some(compaction);
                }
                collect_unique_option_string(
                    &mut session_entry.active_execution_group_ids,
                    resolved_mission_id.clone(),
                );
                collect_unique_option_string(
                    &mut session_entry.active_task_ids,
                    resolved_task_id.clone(),
                );
                collect_unique_payload_value(
                    &mut session_entry.recovery_ids,
                    &event.payload,
                    "recovery_id",
                );
            }
            if let Some(workspace_id) = event.workspace_id.as_ref() {
                let workspace_id = workspace_id.to_string();
                let workspace_entry =
                    workspace_map
                        .entry(workspace_id.clone())
                        .or_insert_with(|| WorkspaceRuntimeSummaryEntry {
                            workspace_id: workspace_id.clone(),
                            ..WorkspaceRuntimeSummaryEntry::default()
                        });
                workspace_entry.event_count += 1;
                if event.category == EventCategory::Audit {
                    workspace_entry.audit_event_count += 1;
                }
                if event.event_type.starts_with("worker.") {
                    workspace_entry.worker_event_count += 1;
                }
                if event.event_type.starts_with("tool.") {
                    workspace_entry.tool_event_count += 1;
                }
                if event.event_type.contains(".resume.") || event.event_type.contains(".resumed.") {
                    workspace_entry.recovery_event_count += 1;
                }
                workspace_entry.latest_event_type = Some(event.event_type.clone());
                collect_unique_option_string(
                    &mut workspace_entry.active_execution_group_ids,
                    resolved_mission_id.clone(),
                );
                collect_unique_option_string(
                    &mut workspace_entry.active_task_ids,
                    resolved_task_id.clone(),
                );
                collect_unique_payload_value(
                    &mut workspace_entry.recovery_ids,
                    &event.payload,
                    "recovery_id",
                );
                collect_unique_payload_value(
                    &mut workspace_entry.execution_chain_refs,
                    &event.payload,
                    "execution_chain_ref",
                );
            }
            if let Some(mission_id) = resolved_mission_id.as_ref() {
                let mission_id = mission_id.to_string();
                if !summary
                    .active_execution_group_ids
                    .iter()
                    .any(|id| id == &mission_id)
                {
                    summary.active_execution_group_ids.push(mission_id.clone());
                }
                let mission_entry = execution_group_map
                    .entry(mission_id.clone())
                    .or_insert_with(|| ExecutionGroupRuntimeSummaryEntry {
                        mission_id: mission_id.clone(),
                        ..ExecutionGroupRuntimeSummaryEntry::default()
                    });
                mission_entry.event_count += 1;
                if event.category == EventCategory::Audit {
                    mission_entry.audit_event_count += 1;
                }
                if event.event_type == "worker.skill_dispatch.applied" {
                    mission_entry.skill_dispatch_count += 1;
                    match event
                        .payload
                        .get("route")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("builtin") => mission_entry.builtin_dispatch_count += 1,
                        Some("bridge") => mission_entry.bridge_dispatch_count += 1,
                        _ => {}
                    }
                    match event
                        .payload
                        .get("status")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("rejected") => mission_entry.rejected_dispatch_count += 1,
                        Some("failed") => mission_entry.failed_dispatch_count += 1,
                        _ => {}
                    }
                }
                if event.event_type == "mission.execution.overview"
                    && let Some(progress) = mission_progress_from_payload(&event.payload)
                {
                    mission_progress_map.insert(mission_id.clone(), progress);
                }
                if event.event_type == "mission.execution.overview"
                    && let Some(context) = event.payload.get("context")
                {
                    mission_entry.context_used_turn_count =
                        nested_usize_field(context, "used_turns");
                    mission_entry.context_used_knowledge_count =
                        nested_usize_field(context, "used_knowledge");
                    mission_entry.context_used_memory_count =
                        nested_usize_field(context, "used_memory");
                    mission_entry.context_used_shared_item_count =
                        nested_usize_field(context, "used_shared_items");
                    mission_entry.context_used_file_summary_count =
                        nested_usize_field(context, "used_file_summaries");
                    mission_entry.context_recent_turn_resolved_count =
                        nested_usize_field(context, "recent_turn_resolved_count");
                    mission_entry.context_recent_turn_retained_count =
                        nested_usize_field(context, "recent_turn_retained_count");
                    mission_entry.context_recent_turn_session_source_count =
                        nested_usize_field(context, "recent_turn_session_source_count");
                    mission_entry.context_recent_turn_project_source_count =
                        nested_usize_field(context, "recent_turn_project_source_count");
                    mission_entry.context_recent_turn_provided_source_count =
                        nested_usize_field(context, "recent_turn_provided_source_count");
                    mission_entry.context_truncation_count =
                        nested_usize_field(context, "truncation_count");
                    mission_entry.context_truncation_parts =
                        nested_string_vec_field(context, "truncation_parts");
                    mission_entry.context_knowledge_ids =
                        nested_string_vec_field(context, "knowledge_ids");
                    mission_entry.context_knowledge_source_paths =
                        nested_string_vec_field(context, "knowledge_source_paths");
                    mission_entry.context_memory_ids =
                        nested_string_vec_field(context, "memory_ids");
                    mission_entry.context_memory_extraction_refs =
                        nested_string_vec_field(context, "memory_extraction_refs");
                    mission_entry.context_shared_context_ids =
                        nested_string_vec_field(context, "shared_context_ids");
                    mission_entry.context_file_summary_paths =
                        nested_string_vec_field(context, "file_summary_paths");
                    mission_entry.context_code_index_knowledge_count =
                        nested_usize_field(context, "code_index_knowledge_count");
                    mission_entry.context_audited_knowledge_count =
                        nested_usize_field(context, "audited_knowledge_count");
                    mission_entry.context_governed_knowledge_count =
                        nested_usize_field(context, "governed_knowledge_count");
                    mission_entry.context_extracted_memory_count =
                        nested_usize_field(context, "extracted_memory_count");
                    mission_entry.context_provenance_linked_memory_count =
                        nested_usize_field(context, "provenance_linked_memory_count");
                }
                mission_entry.latest_event_type = Some(event.event_type.clone());
                if let Some(task_id) = resolved_task_id.as_ref() {
                    let task_id = task_id.to_string();
                    if !mission_entry
                        .active_task_ids
                        .iter()
                        .any(|id| id == &task_id)
                    {
                        mission_entry.active_task_ids.push(task_id);
                    }
                }
            }
            if let Some(task_id) = resolved_task_id.as_ref() {
                let task_id = task_id.to_string();
                if !summary.active_task_ids.iter().any(|id| id == &task_id) {
                    summary.active_task_ids.push(task_id.clone());
                }
                let task_entry =
                    task_map
                        .entry(task_id.clone())
                        .or_insert_with(|| TaskRuntimeSummaryEntry {
                            task_id: task_id.clone(),
                            mission_id: resolved_mission_id.clone(),
                            assignment_id: event.assignment_id.as_ref().map(ToString::to_string),
                            ..TaskRuntimeSummaryEntry::default()
                        });
                task_entry.event_count += 1;
                if event.category == EventCategory::Audit {
                    task_entry.audit_event_count += 1;
                }
                if event.event_type == "worker.skill_dispatch.applied" {
                    task_entry.skill_dispatch_count += 1;
                    match event
                        .payload
                        .get("route")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("builtin") => task_entry.builtin_dispatch_count += 1,
                        Some("bridge") => task_entry.bridge_dispatch_count += 1,
                        _ => {}
                    }
                    match event
                        .payload
                        .get("status")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("rejected") => task_entry.rejected_dispatch_count += 1,
                        Some("failed") => task_entry.failed_dispatch_count += 1,
                        _ => {}
                    }
                }
                task_entry.latest_event_type = Some(event.event_type.clone());
                if let Some(status) = infer_task_status(event) {
                    task_entry.current_status = Some(status);
                }
            }
            if let Some(assignment_id) = event.assignment_id.as_ref() {
                let assignment_id = assignment_id.to_string();
                let assignment_entry =
                    assignment_map
                        .entry(assignment_id.clone())
                        .or_insert_with(|| AssignmentRuntimeSummaryEntry {
                            assignment_id: assignment_id.clone(),
                            mission_id: event.mission_id.as_ref().map(ToString::to_string),
                            ..AssignmentRuntimeSummaryEntry::default()
                        });
                assignment_entry.event_count += 1;
                if event.category == EventCategory::Audit {
                    assignment_entry.audit_event_count += 1;
                }
                if let Some(task_id) = resolved_task_id.as_ref() {
                    let task_id = task_id.to_string();
                    if !assignment_entry.task_ids.iter().any(|id| id == &task_id) {
                        assignment_entry.task_ids.push(task_id);
                    }
                }
                if event.event_type == "task.dispatched"
                    || event.event_type == "mission.resume.dispatch.created"
                {
                    assignment_entry.dispatch_count += 1;
                }
                if let Some(status) = infer_assignment_status(event) {
                    assignment_entry.current_status = Some(status);
                }
                if matches!(
                    event.event_type.as_str(),
                    "task.completed" | "worker.report.applied"
                ) && event.payload.get("status").and_then(|value| value.as_str())
                    == Some("Succeeded")
                {
                    assignment_entry.completed_task_count += 1;
                }
                if matches!(
                    event.event_type.as_str(),
                    "task.failed" | "worker.report.applied"
                ) && matches!(
                    event.payload.get("status").and_then(|value| value.as_str()),
                    Some("Failed") | Some("Blocked")
                ) {
                    assignment_entry.failed_task_count += 1;
                }
                assignment_entry.latest_event_type = Some(event.event_type.clone());
            }
            if let Some(worker_id) = event
                .payload
                .get("worker_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
            {
                let worker_entry = worker_map.entry(worker_id.clone()).or_insert_with(|| {
                    WorkerRuntimeSummaryEntry {
                        worker_id: worker_id.clone(),
                        ..WorkerRuntimeSummaryEntry::default()
                    }
                });
                worker_entry.event_count += 1;
                if event.category == EventCategory::Audit {
                    worker_entry.audit_event_count += 1;
                }
                if event.event_type == "worker.reported" {
                    worker_entry.report_count += 1;
                }
                if event.event_type == "worker.tool.observed" || event.event_type == "tool.invoked"
                {
                    worker_entry.tool_call_count += 1;
                }
                if event.event_type == "worker.skill_dispatch.observed" {
                    worker_entry.skill_dispatch_count += 1;
                    match event
                        .payload
                        .get("route")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("builtin") => worker_entry.builtin_dispatch_count += 1,
                        Some("bridge") => worker_entry.bridge_dispatch_count += 1,
                        _ => {}
                    }
                    match event
                        .payload
                        .get("status")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase())
                        .as_deref()
                    {
                        Some("rejected") => worker_entry.rejected_dispatch_count += 1,
                        Some("failed") => worker_entry.failed_dispatch_count += 1,
                        _ => {}
                    }
                }
                if event.event_type == "worker.executor.observed" {
                    worker_entry.executor_kind = event
                        .payload
                        .get("executor_kind")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_id = event
                        .payload
                        .get("executor_id")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_version = event
                        .payload
                        .get("executor_version")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_instance_id = event
                        .payload
                        .get("executor_instance_id")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_lease_id = event
                        .payload
                        .get("executor_lease_id")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_request_id = event
                        .payload
                        .get("request_id")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_request_source = event
                        .payload
                        .get("request_source")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_observation_status = event
                        .payload
                        .get("observation_status")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_reuse_policy = event
                        .payload
                        .get("requested_reuse_policy")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_binding_scope = event
                        .payload
                        .get("requested_binding_scope")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_lease_state = event
                        .payload
                        .get("requested_lease_state")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_binding_lifecycle = event
                        .payload
                        .get("requested_binding_lifecycle")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_process_lifecycle = event
                        .payload
                        .get("requested_process_lifecycle")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_process_model = event
                        .payload
                        .get("requested_process_model")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_requested_parallelism = event
                        .payload
                        .get("requested_parallelism")
                        .and_then(|value| value.as_u64())
                        .map(|value| value as usize);
                    worker_entry.executor_requested_step_kinds = event
                        .payload
                        .get("requested_step_kinds")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    worker_entry.executor_execution_mode = event
                        .payload
                        .get("execution_mode")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_process_model = event
                        .payload
                        .get("process_model")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_lease_state = event
                        .payload
                        .get("lease_state")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_binding_lifecycle = event
                        .payload
                        .get("binding_lifecycle")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_process_lifecycle = event
                        .payload
                        .get("process_lifecycle")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_reuse_scope = event
                        .payload
                        .get("reuse_scope")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_parallelism_scope = event
                        .payload
                        .get("parallelism_scope")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_health_status = event
                        .payload
                        .get("health_status")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_ascii_lowercase());
                    worker_entry.executor_failure_layer = event
                        .payload
                        .get("failure_layer")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string);
                    worker_entry.executor_failure_message = event
                        .payload
                        .get("failure_message")
                        .and_then(|value| value.as_str())
                        .and_then(|value| public_runtime_summary(Some(value)));
                    worker_entry.executor_supported_step_kinds = event
                        .payload
                        .get("supported_step_kinds")
                        .and_then(|value| value.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                }
                worker_entry.latest_event_type = Some(event.event_type.clone());
                if let Some(task_id) = event
                    .payload
                    .get("task_id")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
                {
                    worker_entry.current_task_id = Some(task_id);
                } else if let Some(task_id) = event.task_id.as_ref() {
                    worker_entry.current_task_id = Some(task_id.to_string());
                }
                if let Some(status) = infer_worker_status(event) {
                    worker_entry.current_status = Some(status);
                }
                if let Some(stage) = infer_worker_stage(event) {
                    worker_entry.current_stage = Some(stage);
                }
            }
            if event.event_type == "task.dispatched"
                || event.event_type == "mission.resume.dispatch.created"
            {
                dispatch.total_dispatches += 1;
                if let Some(assignment_id) = event.assignment_id.as_ref() {
                    let assignment_id = assignment_id.to_string();
                    if !dispatch
                        .active_assignment_ids
                        .iter()
                        .any(|id| id == &assignment_id)
                    {
                        dispatch.active_assignment_ids.push(assignment_id);
                    }
                }
                if event.event_type == "mission.resume.dispatch.created" {
                    dispatch.resume_dispatches += 1;
                }
                if let Some(dispatch_reason) = event
                    .payload
                    .get("dispatch_reason")
                    .and_then(|value| value.as_str())
                {
                    dispatch.latest_dispatch_reason = Some(dispatch_reason.to_ascii_lowercase());
                }
            }
            if event.event_type == "tool.invoked" {
                let tool_name = event
                    .payload
                    .get("tool_name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let tool_entry =
                    tool_map
                        .entry(tool_name.clone())
                        .or_insert_with(|| ToolRuntimeSummaryEntry {
                            tool_name: tool_name.clone(),
                            ..ToolRuntimeSummaryEntry::default()
                        });
                tool_entry.event_count += 1;
                tool_entry.latest_event_type = Some(event.event_type.clone());
                tool_entry.tool_kind = event
                    .payload
                    .get("tool_kind")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string);
                if let Some(status) = event
                    .payload
                    .get("status")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_ascii_lowercase())
                {
                    match status.as_str() {
                        "succeeded" => tool_entry.success_count += 1,
                        "needsapproval" | "rejected" => tool_entry.blocked_count += 1,
                        "failed" => tool_entry.failed_count += 1,
                        _ => {}
                    }
                    tool_entry.latest_status = Some(status);
                }
                collect_unique_payload_value(
                    &mut tool_entry.worker_ids,
                    &event.payload,
                    "worker_id",
                );
                collect_unique_payload_value(&mut tool_entry.task_ids, &event.payload, "task_id");
                collect_unique_payload_value(
                    &mut tool_entry.session_ids,
                    &event.payload,
                    "session_id",
                );
                collect_unique_payload_value(
                    &mut tool_entry.workspace_ids,
                    &event.payload,
                    "workspace_id",
                );
            }
            if event.event_type == "governance.decision.applied" {
                governance_total_count += 1;
                match event
                    .payload
                    .get("status")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_ascii_lowercase())
                    .as_deref()
                {
                    Some("allowed") => governance_allowed_count += 1,
                    Some("needs_approval") => governance_needs_approval_count += 1,
                    Some("blocked") => governance_blocked_count += 1,
                    Some("rejected") => governance_rejected_count += 1,
                    _ => {}
                }
                collect_unique_payload_value(
                    &mut governance_blocked_task_ids,
                    &event.payload,
                    "task_id",
                );
                collect_unique_payload_value(
                    &mut governance_approval_required_task_ids,
                    &event.payload,
                    "task_id",
                );
                collect_unique_payload_value(
                    &mut governance_rejected_task_ids,
                    &event.payload,
                    "task_id",
                );
                collect_unique_payload_value(
                    &mut governance_blocked_worker_ids,
                    &event.payload,
                    "worker_id",
                );
                collect_unique_payload_value(
                    &mut governance_approval_required_worker_ids,
                    &event.payload,
                    "worker_id",
                );
                collect_unique_payload_value(
                    &mut governance_rejected_worker_ids,
                    &event.payload,
                    "worker_id",
                );
            }
        }

        let recovery = RecoveryReadModelInput::from_events(events);
        refresh_execution_group_statuses(
            &mut execution_group_map,
            &task_map,
            &recovery,
            &mission_progress_map,
        );

        let execution_groups = execution_group_map.into_values().collect::<Vec<_>>();
        let tasks = task_map.into_values().collect::<Vec<_>>();
        let assignments = assignment_map.into_values().collect::<Vec<_>>();
        let workers = worker_map.into_values().collect::<Vec<_>>();
        let tools = tool_map.into_values().collect::<Vec<_>>();
        let sessions = session_map.into_values().collect::<Vec<_>>();
        let workspaces = workspace_map.into_values().collect::<Vec<_>>();
        knowledge_audit.sort_by_key(|entry| entry.sequence);
        if knowledge_audit.len() > 64 {
            knowledge_audit.drain(..knowledge_audit.len() - 64);
        }
        let aggregate_components = RuntimeAggregateComponents {
            execution_groups: &execution_groups,
            tasks: &tasks,
            assignments: &assignments,
            workers: &workers,
            tools: &tools,
            recovery: &recovery,
        };
        let diagnostics = RuntimeDiagnosticSummary::from_components(
            &aggregate_components,
            GovernanceDiagnosticCounts {
                total: governance_total_count,
                allowed: governance_allowed_count,
                needs_approval: governance_needs_approval_count,
                blocked: governance_blocked_count,
                rejected: governance_rejected_count,
            },
        );
        let attention = RuntimeAttentionSummary::from_components(
            &aggregate_components,
            GovernanceAttentionIds {
                blocked_task_ids: &governance_blocked_task_ids,
                approval_required_task_ids: &governance_approval_required_task_ids,
                rejected_task_ids: &governance_rejected_task_ids,
                blocked_worker_ids: &governance_blocked_worker_ids,
                approval_required_worker_ids: &governance_approval_required_worker_ids,
                rejected_worker_ids: &governance_rejected_worker_ids,
            },
        );
        let work_queues = RuntimeWorkQueueSummary::from_components(
            &execution_groups,
            &tasks,
            &assignments,
            &workers,
            &recovery,
        );
        let resume_observation = RecoveryResumeObservationSummary::from_recovery(&recovery);
        let operations = RuntimeOperationsSummary {
            dispatch,
            attention,
            work_queues,
            resume_observation,
        };

        let overview = RuntimeOverviewSummary {
            category_counts: category_counts.clone(),
            activity: summary,
            diagnostics,
        };
        let details = RuntimeDetailsSummary {
            execution_groups,
            tasks,
            assignments,
            workers,
            tools,
            sessions,
            workspaces,
            knowledge_audit,
        };

        let meta = RuntimeMetaSummary {
            contract_version: RUNTIME_READ_MODEL_CONTRACT_VERSION.to_string(),
            contract_sections: runtime_read_model_contract_sections(),
            ordering_strategy: RUNTIME_READ_MODEL_ORDERING_STRATEGY.to_string(),
            section_ordering_rules: runtime_read_model_section_ordering_rules(),
            ledger: RuntimeLedgerSummary {
                schema_version: RUNTIME_LEDGER_SCHEMA_VERSION.to_string(),
                audit_count: category_counts.audit,
                usage_count: category_counts.usage,
                next_sequence: latest_sequence.saturating_add(1).max(1),
                persistence_path: None,
                last_persist_error: None,
                is_persist_healthy: true,
                last_persisted_at: None,
                pending_flush: false,
                readiness: RuntimeLedgerReadinessSummary::default(),
                cutover_readiness: RuntimeLedgerReadinessSummary::default(),
            },
            maintenance: RuntimeMaintenanceSummary::from_events(events),
            executor: RuntimeExecutorSummary::from_events(events),
            freeze: RuntimeContractFreezeSummary::default(),
            freeze_gate: RuntimeContractFreezeGateSummary::default(),
            freeze_evidence: RuntimeContractFreezeEvidenceSummary::default(),
            freeze_report: RuntimeContractFreezeReportSummary::default(),
            freeze_consistency: RuntimeContractFreezeConsistencySummary::default(),
            freeze_closure: RuntimeContractFreezeClosureSummary::default(),
            validation: RuntimeContractValidationSummary::default(),
            latest_sequence,
            recent_event_count,
        };

        let mut read_model = Self {
            meta,
            overview,
            details,
            operations,
            recovery,
        }
        .normalize();
        read_model.refresh_contract_state();
        read_model.overview.diagnostics.governance_total_count = governance_total_count;
        read_model.overview.diagnostics.governance_allowed_count = governance_allowed_count;
        read_model
            .overview
            .diagnostics
            .governance_needs_approval_count = governance_needs_approval_count;
        read_model.overview.diagnostics.governance_blocked_count = governance_blocked_count;
        read_model.overview.diagnostics.governance_rejected_count = governance_rejected_count;
        read_model.operations.attention.governance_blocked_task_ids = governance_blocked_task_ids;
        read_model
            .operations
            .attention
            .governance_approval_required_task_ids = governance_approval_required_task_ids;
        read_model.operations.attention.governance_rejected_task_ids = governance_rejected_task_ids;
        read_model
            .operations
            .attention
            .governance_blocked_worker_ids = governance_blocked_worker_ids;
        read_model
            .operations
            .attention
            .governance_approval_required_worker_ids = governance_approval_required_worker_ids;
        read_model
            .operations
            .attention
            .governance_rejected_worker_ids = governance_rejected_worker_ids;
        read_model
    }

    fn normalize(mut self) -> Self {
        sort_string_vec(&mut self.overview.activity.active_execution_group_ids);
        sort_string_vec(&mut self.overview.activity.active_task_ids);
        sort_string_vec(&mut self.meta.executor.supported_step_kinds);

        self.details
            .execution_groups
            .sort_by(|left, right| left.mission_id.cmp(&right.mission_id));
        self.details
            .tasks
            .sort_by(|left, right| left.task_id.cmp(&right.task_id));
        self.details
            .assignments
            .sort_by(|left, right| left.assignment_id.cmp(&right.assignment_id));
        self.details
            .workers
            .sort_by(|left, right| left.worker_id.cmp(&right.worker_id));
        self.details
            .tools
            .sort_by(|left, right| left.tool_name.cmp(&right.tool_name));
        self.details
            .sessions
            .sort_by(|left, right| left.session_id.cmp(&right.session_id));
        self.details
            .workspaces
            .sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
        self.details
            .knowledge_audit
            .sort_by_key(|entry| entry.sequence);

        for entry in &mut self.details.execution_groups {
            sort_string_vec(&mut entry.active_task_ids);
            sort_string_vec(&mut entry.context_truncation_parts);
            sort_string_vec(&mut entry.context_knowledge_ids);
            sort_string_vec(&mut entry.context_knowledge_source_paths);
            sort_string_vec(&mut entry.context_memory_ids);
            sort_string_vec(&mut entry.context_memory_extraction_refs);
            sort_string_vec(&mut entry.context_shared_context_ids);
            sort_string_vec(&mut entry.context_file_summary_paths);
        }
        for entry in &mut self.details.assignments {
            sort_string_vec(&mut entry.task_ids);
        }
        for entry in &mut self.details.tools {
            sort_string_vec(&mut entry.worker_ids);
            sort_string_vec(&mut entry.task_ids);
            sort_string_vec(&mut entry.session_ids);
            sort_string_vec(&mut entry.workspace_ids);
        }
        for entry in &mut self.details.sessions {
            sort_string_vec(&mut entry.active_execution_group_ids);
            sort_string_vec(&mut entry.active_task_ids);
            sort_string_vec(&mut entry.recovery_ids);
        }
        for entry in &mut self.details.workspaces {
            sort_string_vec(&mut entry.active_execution_group_ids);
            sort_string_vec(&mut entry.active_task_ids);
            sort_string_vec(&mut entry.recovery_ids);
            sort_string_vec(&mut entry.execution_chain_refs);
        }
        for entry in &mut self.details.workers {
            sort_string_vec(&mut entry.executor_supported_step_kinds);
            sort_string_vec(&mut entry.executor_requested_step_kinds);
        }

        sort_string_vec(&mut self.operations.dispatch.active_assignment_ids);
        sort_string_vec(&mut self.operations.attention.failed_execution_group_ids);
        sort_string_vec(&mut self.operations.attention.failed_task_ids);
        sort_string_vec(&mut self.operations.attention.failed_assignment_ids);
        sort_string_vec(&mut self.operations.attention.failed_worker_ids);
        sort_string_vec(&mut self.operations.attention.blocked_tool_names);
        sort_string_vec(&mut self.operations.attention.governance_blocked_task_ids);
        sort_string_vec(
            &mut self
                .operations
                .attention
                .governance_approval_required_task_ids,
        );
        sort_string_vec(&mut self.operations.attention.governance_rejected_task_ids);
        sort_string_vec(&mut self.operations.attention.governance_blocked_worker_ids);
        sort_string_vec(
            &mut self
                .operations
                .attention
                .governance_approval_required_worker_ids,
        );
        sort_string_vec(&mut self.operations.attention.governance_rejected_worker_ids);
        sort_string_vec(&mut self.operations.attention.rejected_skill_dispatch_worker_ids);
        sort_string_vec(&mut self.operations.attention.failed_skill_dispatch_worker_ids);
        sort_string_vec(&mut self.operations.attention.degraded_executor_worker_ids);
        sort_string_vec(&mut self.operations.attention.unavailable_executor_worker_ids);
        sort_string_vec(&mut self.operations.attention.pending_recovery_ids);
        sort_string_vec(&mut self.operations.work_queues.running_execution_group_ids);
        sort_string_vec(&mut self.operations.work_queues.running_task_ids);
        sort_string_vec(&mut self.operations.work_queues.running_assignment_ids);
        sort_string_vec(&mut self.operations.work_queues.active_worker_ids);
        sort_string_vec(&mut self.operations.work_queues.pending_recovery_ids);
        sort_string_vec(
            &mut self
                .operations
                .resume_observation
                .affected_execution_group_ids,
        );
        sort_string_vec(&mut self.operations.resume_observation.affected_worker_ids);

        self.recovery = self.recovery.normalize();
        self
    }
}

fn infer_task_status(event: &EventEnvelope) -> Option<String> {
    if event.event_type == "task.status.changed"
        && let Some(status) = event
            .payload
            .get("new_status")
            .and_then(|value| value.as_str())
    {
        return Some(status.to_ascii_lowercase());
    }
    if let Some(status) = event.payload.get("status").and_then(|value| value.as_str()) {
        return Some(status.to_ascii_lowercase());
    }
    match event.event_type.as_str() {
        "task.created" => Some("pending".to_string()),
        "task.dispatched" => Some("running".to_string()),
        "mission.resume.dispatch.created"
        | "mission.resumed.from_recovery"
        | "worker.resumed.from_recovery"
        | "worker.resumed.from_dispatch" => Some("running".to_string()),
        _ => None,
    }
}

fn event_task_id(event: &EventEnvelope) -> Option<String> {
    event
        .task_id
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            event
                .payload
                .get("task_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .payload
                .get("taskId")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
}

fn event_mission_id(event: &EventEnvelope) -> Option<String> {
    event
        .mission_id
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            event
                .payload
                .get("mission_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .payload
                .get("missionId")
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
        })
}

fn infer_assignment_status(event: &EventEnvelope) -> Option<String> {
    match event.event_type.as_str() {
        "assignment.created" => Some("pending".to_string()),
        "task.dispatched" | "mission.resume.dispatch.created" => Some("running".to_string()),
        "task.completed" | "worker.report.applied" => {
            match event.payload.get("status").and_then(|value| value.as_str()) {
                Some("Succeeded") => Some("succeeded".to_string()),
                Some("Failed") | Some("Blocked") => Some("failed".to_string()),
                Some("Running") => Some("running".to_string()),
                _ => None,
            }
        }
        "task.failed" => Some("failed".to_string()),
        _ => None,
    }
}

fn mission_progress_from_payload(payload: &serde_json::Value) -> Option<MissionProgressSummary> {
    let total_tasks = payload.get("total_tasks")?.as_u64()? as usize;
    let completed_tasks = payload
        .get("completed_tasks")
        .and_then(|value| value.as_u64())
        .unwrap_or_default() as usize;
    let failed_tasks = payload
        .get("failed_tasks")
        .and_then(|value| value.as_u64())
        .unwrap_or_default() as usize;
    Some(MissionProgressSummary {
        total_tasks,
        completed_tasks,
        failed_tasks,
    })
}

fn refresh_execution_group_statuses(
    execution_group_map: &mut BTreeMap<String, ExecutionGroupRuntimeSummaryEntry>,
    task_map: &BTreeMap<String, TaskRuntimeSummaryEntry>,
    recovery: &RecoveryReadModelInput,
    mission_progress_map: &BTreeMap<String, MissionProgressSummary>,
) {
    for execution_group_entry in execution_group_map.values_mut() {
        execution_group_entry.current_status = derive_execution_group_status(
            execution_group_entry,
            task_map,
            recovery,
            mission_progress_map
                .get(&execution_group_entry.mission_id)
                .copied(),
        );
    }
}

fn derive_execution_group_status(
    execution_group_entry: &ExecutionGroupRuntimeSummaryEntry,
    task_map: &BTreeMap<String, TaskRuntimeSummaryEntry>,
    recovery: &RecoveryReadModelInput,
    progress: Option<MissionProgressSummary>,
) -> Option<String> {
    if recovery.summaries.iter().any(|summary| {
        summary
            .mission_id
            .as_ref()
            .map(|mission_id| mission_id.as_str())
            == Some(execution_group_entry.mission_id.as_str())
            && summary.current_status == "resuming"
    }) {
        return Some("resuming".to_string());
    }

    let task_statuses = task_map
        .values()
        .filter(|task| {
            task.mission_id.as_deref() == Some(execution_group_entry.mission_id.as_str())
        })
        .filter_map(|task| task.current_status.as_deref())
        .collect::<Vec<_>>();

    if !task_statuses.is_empty() {
        if task_statuses
            .iter()
            .any(|status| is_running_task_status(status))
        {
            return Some("running".to_string());
        }
        if task_statuses
            .iter()
            .any(|status| is_failed_task_status(status))
        {
            return Some("failed".to_string());
        }
        if task_statuses
            .iter()
            .all(|status| is_terminal_task_status(status))
        {
            return Some("succeeded".to_string());
        }
        return Some("pending".to_string());
    }

    progress.and_then(derive_mission_status_from_progress)
}

fn derive_mission_status_from_progress(progress: MissionProgressSummary) -> Option<String> {
    if progress.total_tasks == 0 {
        return None;
    }
    if progress.completed_tasks >= progress.total_tasks {
        return Some("succeeded".to_string());
    }
    if progress.failed_tasks > 0 {
        return Some("failed".to_string());
    }
    if progress.completed_tasks > 0 {
        return Some("running".to_string());
    }
    Some("pending".to_string())
}

fn is_running_task_status(status: &str) -> bool {
    matches!(status, "running" | "verifying" | "repairing")
}

fn is_failed_task_status(status: &str) -> bool {
    matches!(status, "failed" | "blocked")
}

fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "succeeded" | "cancelled" | "skipped")
}

fn infer_worker_status(event: &EventEnvelope) -> Option<String> {
    if let Some(status) = event.payload.get("status").and_then(|value| value.as_str()) {
        return Some(status.to_ascii_lowercase());
    }
    match event.event_type.as_str() {
        "worker.registered" => Some("idle".to_string()),
        "worker.resumed.from_recovery" | "worker.resumed.from_dispatch" => {
            Some("running".to_string())
        }
        "worker.reported" => event
            .payload
            .get("result_kind")
            .and_then(|value| value.as_str())
            .map(|value| match value {
                "Success" => "finished".to_string(),
                "Failure" => "failed".to_string(),
                _ => "running".to_string(),
            }),
        _ => None,
    }
}

fn infer_worker_stage(event: &EventEnvelope) -> Option<String> {
    event
        .payload
        .get("stage")
        .and_then(|value| value.as_str())
        .map(|value| value.to_ascii_lowercase())
}

/// 从 `model.usage.recorded` 事件提取上下文窗口观测值。
///
/// payload 是 camelCase 序列化的 `UsageCallRecordInput`。仅采集成功调用
/// （`status == "success"`）的当前请求上下文窗口 token 与解析模型名，
/// 窗口大小的推断交由 magi-api 装配层完成。
fn usage_observation_from_event(event: &EventEnvelope) -> Option<SessionRuntimeUsageObservation> {
    if event.event_type == "session.context.usage.updated" {
        let context_window_tokens = event
            .payload
            .get("token_used")
            .and_then(serde_json::Value::as_u64)?;
        if context_window_tokens == 0 {
            return None;
        }
        return Some(SessionRuntimeUsageObservation {
            context_window_tokens,
            resolved_model: event
                .payload
                .get("resolved_model")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string),
            observed_at: event
                .payload
                .get("updated_at")
                .and_then(serde_json::Value::as_u64)
                .map(UtcMillis)
                .or(Some(event.occurred_at)),
            measurement: event
                .payload
                .get("accuracy")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            phase: event
                .payload
                .get("phase")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            turn_id: event
                .payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            call_id: event
                .payload
                .get("call_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }
    usage_observation_from_payload(&event.event_type, &event.payload)
}

fn context_compaction_from_event(
    event: &EventEnvelope,
) -> Option<SessionRuntimeContextCompactionEntry> {
    if event.event_type != "session.context.compacted" {
        return None;
    }
    let payload = &event.payload;
    let reason = payload
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let compacted_at = payload
        .get("compacted_at")
        .and_then(serde_json::Value::as_u64)
        .map(UtcMillis)
        .or(Some(event.occurred_at));

    Some(SessionRuntimeContextCompactionEntry {
        reason,
        phase: payload
            .get("phase")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        original_message_count: payload
            .get("original_message_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        compacted_message_count: payload
            .get("compacted_message_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        original_token_estimate: payload
            .get("original_token_estimate")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        compacted_token_estimate: payload
            .get("compacted_token_estimate")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        context_window_tokens: payload
            .get("context_window_tokens")
            .and_then(serde_json::Value::as_u64),
        token_limit: payload
            .get("token_limit")
            .and_then(serde_json::Value::as_u64),
        threshold_tokens: payload
            .get("threshold_tokens")
            .and_then(serde_json::Value::as_u64),
        resolved_model: payload
            .get("resolved_model")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        compacted_at,
    })
}

/// 从 `model.usage.recorded` 负载提取上下文窗口观测值。
///
/// 反孤儿:既服务于实时 `recent_events` 投影,也服务于守护进程重启后从
/// 审计/用量账本回放的路径(`latest_usage_observations_from_ledger`),两条
/// 路径共用同一口径,避免重启前后预算计算出现漂移。
///
/// 口径对齐 Codex:展示用的上下文窗口占用取最近一次模型调用返回的
/// `totalTokens`；没有显式 total 时用 raw input + raw output。这里不扣
/// cache read,因为缓存命中仍然占用模型上下文窗口,只是不按同样价格计费。
///
/// 输入区圆环展示主线 orchestrator 的上下文窗口。worker / auxiliary 的模型
/// 调用仍进入审计账本与任务指标,但不能覆盖主线会话窗口统计。
fn usage_observation_from_payload(
    event_type: &str,
    payload: &serde_json::Value,
) -> Option<SessionRuntimeUsageObservation> {
    if event_type != "model.usage.recorded" {
        return None;
    }
    if payload.get("status").and_then(|value| value.as_str()) != Some("success") {
        return None;
    }
    if !is_orchestrator_usage_payload(payload) {
        return None;
    }
    let usage = payload.get("usage")?;
    let input_tokens = usage
        .get("inputTokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("totalTokens")
        .or_else(|| usage.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let context_window_tokens = if total_tokens > 0 {
        total_tokens
    } else {
        input_tokens.saturating_add(output_tokens)
    };
    if context_window_tokens == 0 {
        return None;
    }
    let resolved_model = payload
        .get("modelConfig")
        .and_then(|config| config.get("model"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let observed_at = payload
        .get("timestamp")
        .and_then(serde_json::Value::as_u64)
        .map(UtcMillis);
    Some(SessionRuntimeUsageObservation {
        context_window_tokens,
        resolved_model,
        observed_at,
        measurement: Some("authoritative".to_string()),
        phase: Some("completed".to_string()),
        turn_id: payload
            .get("turnId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        call_id: payload
            .get("callIdentity")
            .and_then(|identity| identity.get("callId"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

fn is_orchestrator_usage_payload(payload: &serde_json::Value) -> bool {
    payload
        .get("executionBinding")
        .and_then(|binding| binding.get("role"))
        .and_then(serde_json::Value::as_str)
        == Some("orchestrator")
}

/// 从已恢复的审计/用量账本条目重建每会话最近一次用量观测值。
///
/// 反孤儿/重启容错:守护进程重启后实时 `recent_events` 缓冲区从空开始,
/// `model.usage.recorded` 事件只存在于持久化的用量账本中。读模型按 sidecar
/// 重建会话条目却拿不到观测值,导致预算整体丢失。此函数按账本顺序(`sequence`)
/// 回放,保留每个会话的最后一次成功观测,供 DTO 装配层回填 `usage_observation`
/// 后再计算预算。
pub fn latest_usage_observations_from_ledger(
    usage_entries: &[crate::AuditUsageLedgerEntry],
) -> BTreeMap<String, SessionRuntimeUsageObservation> {
    let mut latest: BTreeMap<String, ((u64, u64), SessionRuntimeUsageObservation)> =
        BTreeMap::new();
    for entry in usage_entries {
        let Some(session_id) = entry.context.session_id.as_ref() else {
            continue;
        };
        let Some(observation) = usage_observation_from_payload(&entry.event_type, &entry.payload)
        else {
            continue;
        };
        let ordering_key = (
            observation.observed_at.map(|value| value.0).unwrap_or(0),
            entry.sequence,
        );
        let session_id = session_id.to_string();
        match latest.get(&session_id) {
            Some((existing_key, _)) if *existing_key >= ordering_key => {}
            _ => {
                latest.insert(session_id, (ordering_key, observation));
            }
        }
    }
    latest
        .into_iter()
        .map(|(session_id, (_, observation))| (session_id, observation))
        .collect()
}

fn nested_usize_field(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or_default()
}

fn nested_string_vec_field(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn collect_unique_payload_value(target: &mut Vec<String>, payload: &serde_json::Value, key: &str) {
    if let Some(value) = payload.get(key).and_then(|value| value.as_str()) {
        let value = value.to_string();
        if !target.iter().any(|existing| existing == &value) {
            target.push(value);
        }
    }
}

fn collect_unique_option_string(target: &mut Vec<String>, value: Option<String>) {
    if let Some(value) = value
        && !target.iter().any(|existing| existing == &value)
    {
        target.push(value);
    }
}

fn sort_string_vec(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn infer_recovery_status(stage: RecoveryActivityStage) -> String {
    match stage {
        RecoveryActivityStage::ResumeCommandCreated
        | RecoveryActivityStage::ResumeDispatchCreated => "resuming".to_string(),
        RecoveryActivityStage::MissionResumed => "mission_resumed".to_string(),
        RecoveryActivityStage::WorkerResumed => "worker_resumed".to_string(),
    }
}

impl RecoveryActivityEntry {
    pub fn from_event(event: &EventEnvelope) -> Option<Self> {
        let stage = match event.event_type.as_str() {
            "mission.resume.command.created" => RecoveryActivityStage::ResumeCommandCreated,
            "mission.resume.dispatch.created" => RecoveryActivityStage::ResumeDispatchCreated,
            "mission.resumed.from_recovery" => RecoveryActivityStage::MissionResumed,
            "worker.resumed.from_recovery" | "worker.resumed.from_dispatch" => {
                RecoveryActivityStage::WorkerResumed
            }
            _ => return None,
        };

        let recovery_id = event.payload.get("recovery_id")?.as_str()?.to_string();
        Some(Self {
            recovery_id,
            stage,
            event_type: event.event_type.clone(),
            category: event.category,
            occurred_at: event.occurred_at,
            sequence: event.sequence,
            workspace_id: event.workspace_id.clone(),
            session_id: event.session_id.clone(),
            mission_id: event.mission_id.clone(),
            assignment_id: event.assignment_id.clone(),
            task_id: event.task_id.clone(),
            worker_id: event
                .payload
                .get("worker_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
            execution_chain_ref: event
                .payload
                .get("execution_chain_ref")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
            diagnostic_summary: event
                .payload
                .get("diagnostic_summary")
                .and_then(|value| value.as_str())
                .and_then(|value| public_runtime_summary(Some(value))),
        })
    }
}

impl RecoveryDiagnosticSummaryEntry {
    fn from_entry(entry: &RecoveryActivityEntry) -> Self {
        Self {
            recovery_id: entry.recovery_id.clone(),
            event_count: 0,
            latest_stage: entry.stage,
            latest_event_type: entry.event_type.clone(),
            latest_sequence: entry.sequence,
            latest_occurred_at: entry.occurred_at,
            workspace_id: entry.workspace_id.clone(),
            session_id: entry.session_id.clone(),
            mission_id: entry.mission_id.clone(),
            assignment_id: entry.assignment_id.clone(),
            task_id: entry.task_id.clone(),
            worker_id: entry.worker_id.clone(),
            execution_chain_ref: entry.execution_chain_ref.clone(),
            diagnostic_summary: entry.diagnostic_summary.clone(),
            current_status: infer_recovery_status(entry.stage),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventContext;
    use magi_core::{EventId, SessionId, WorkspaceId};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn runtime_ledger公开摘要不暴露内部持久化错误() {
        let summary = RuntimeLedgerSummary::from(AuditUsageLedgerStatus {
            schema_version: RUNTIME_LEDGER_SCHEMA_VERSION.to_string(),
            next_sequence: 1,
            audit_count: 0,
            usage_count: 0,
            persistence_path: Some(PathBuf::from("/Users/xie/.magi/audit-usage-ledger.json")),
            last_persist_error: Some(
                "/Users/xie/.magi/audit-usage-ledger.json: Permission denied".to_string(),
            ),
        });

        assert_eq!(
            summary.last_persist_error.as_deref(),
            Some(RUNTIME_LEDGER_PERSIST_ERROR_SUMMARY)
        );
        let public_error = summary
            .last_persist_error
            .as_deref()
            .expect("public error should exist");
        assert!(!public_error.contains("/Users/xie"));
        assert!(!public_error.contains("Permission denied"));
        assert!(!summary.is_persist_healthy);
    }

    #[test]
    fn recovery_diagnostic_summary进入读模型前会脱敏() {
        let mut event = EventEnvelope::domain(
            EventId::new("event-recovery-diagnostic"),
            "mission.resume.command.created",
            json!({
                "recovery_id": "recovery-public-diagnostic",
                "diagnostic_summary": "resume failed at /Users/xie/.magi/session.json with Bearer abcdef and sk-live-secret"
            }),
        );
        event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[event]);
        let entry_summary = read_model
            .recovery
            .entries
            .first()
            .and_then(|entry| entry.diagnostic_summary.as_deref())
            .expect("recovery entry diagnostic should exist");
        let aggregate_summary = read_model
            .recovery
            .summaries
            .first()
            .and_then(|summary| summary.diagnostic_summary.as_deref())
            .expect("recovery aggregate diagnostic should exist");

        for public_summary in [entry_summary, aggregate_summary] {
            assert!(public_summary.contains("[path]"));
            assert!(public_summary.contains("Bearer [redacted]"));
            assert!(public_summary.contains("sk-[redacted]"));
            assert!(!public_summary.contains("/Users/xie"));
            assert!(!public_summary.contains("abcdef"));
            assert!(!public_summary.contains("sk-live-secret"));
        }
    }

    #[test]
    fn executor自由文本进入读模型前会脱敏() {
        let mut event = EventEnvelope::audit(
            EventId::new("event-executor-sensitive-text"),
            "worker.executor.observed",
            json!({
                "worker_id": "worker-sensitive-executor",
                "task_id": "task-sensitive-executor",
                "observation_status": "unavailable",
                "execution_mode": "local-process",
                "executor_id": "local-process-worker-executor",
                "executor_version": "worker-local-process-executor-v2",
                "health_status": "Unavailable",
                "health_detail": "probe failed at /Users/xie/.magi/executor.json with Bearer abcdef and sk-health-secret",
                "failure_layer": "transport",
                "failure_message": "spawn failed at /private/tmp/magi/executor with Bearer ghijkl and sk-failure-secret",
                "supported_step_kinds": ["final-report"],
                "max_parallelism": 1,
                "observed_at": 1
            }),
        );
        event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[event]);
        let health_detail = read_model
            .meta
            .executor
            .health_detail
            .as_deref()
            .expect("executor health detail should exist");
        let failure_message = read_model
            .meta
            .executor
            .failure_message
            .as_deref()
            .expect("executor failure message should exist");
        let worker_failure_message = read_model
            .details
            .workers
            .first()
            .and_then(|worker| worker.executor_failure_message.as_deref())
            .expect("worker executor failure message should exist");

        for public_summary in [health_detail, failure_message, worker_failure_message] {
            assert!(public_summary.contains("[path]"));
            assert!(public_summary.contains("Bearer [redacted]"));
            assert!(public_summary.contains("sk-[redacted]"));
            assert!(!public_summary.contains("/Users/xie"));
            assert!(!public_summary.contains("/private/tmp"));
            assert!(!public_summary.contains("abcdef"));
            assert!(!public_summary.contains("ghijkl"));
            assert!(!public_summary.contains("sk-health-secret"));
            assert!(!public_summary.contains("sk-failure-secret"));
        }
    }

    #[test]
    fn mission_execution_overview_context_summary_updates_runtime_read_model() {
        let mut event = EventEnvelope::audit(
            EventId::new("event-1"),
            "mission.execution.overview",
            json!({
                "mission_id": "mission-1",
                "total_tasks": 1,
                "completed_tasks": 0,
                "failed_tasks": 0,
                "context": {
                    "used_turns": 1,
                    "used_knowledge": 2,
                    "used_memory": 3,
                    "used_shared_items": 1,
                    "used_file_summaries": 1,
                    "recent_turn_resolved_count": 3,
                    "recent_turn_retained_count": 2,
                    "recent_turn_session_source_count": 1,
                    "recent_turn_project_source_count": 1,
                    "recent_turn_provided_source_count": 0,
                    "truncation_count": 2,
                    "truncation_parts": ["memory", "knowledge"],
                    "knowledge_ids": ["kb-b", "kb-a"],
                    "knowledge_source_paths": ["src/b.rs", "src/a.rs"],
                    "memory_ids": ["mem-b", "mem-a"],
                    "memory_extraction_refs": ["extract-b", "extract-a"],
                    "shared_context_ids": ["shared-b", "shared-a"],
                    "file_summary_paths": ["/tmp/b.rs", "/tmp/a.rs"],
                    "code_index_knowledge_count": 1,
                    "audited_knowledge_count": 1,
                    "governed_knowledge_count": 1,
                    "extracted_memory_count": 2,
                    "provenance_linked_memory_count": 3
                }
            }),
        )
        .with_context(EventContext {
            mission_id: Some(MissionId::new("mission-1")),
            ..EventContext::default()
        });
        event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[event]);
        let mission = read_model
            .details
            .execution_groups
            .first()
            .expect("execution group entry to exist");

        assert_eq!(mission.context_used_turn_count, 1);
        assert_eq!(mission.context_used_knowledge_count, 2);
        assert_eq!(mission.context_used_memory_count, 3);
        assert_eq!(mission.context_used_shared_item_count, 1);
        assert_eq!(mission.context_used_file_summary_count, 1);
        assert_eq!(mission.context_recent_turn_resolved_count, 3);
        assert_eq!(mission.context_recent_turn_retained_count, 2);
        assert_eq!(mission.context_recent_turn_session_source_count, 1);
        assert_eq!(mission.context_recent_turn_project_source_count, 1);
        assert_eq!(mission.context_recent_turn_provided_source_count, 0);
        assert_eq!(mission.context_truncation_count, 2);
        assert_eq!(
            mission.context_truncation_parts,
            vec!["knowledge".to_string(), "memory".to_string()]
        );
        assert_eq!(
            mission.context_knowledge_ids,
            vec!["kb-a".to_string(), "kb-b".to_string()]
        );
        assert_eq!(
            mission.context_knowledge_source_paths,
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
        assert_eq!(
            mission.context_memory_ids,
            vec!["mem-a".to_string(), "mem-b".to_string()]
        );
        assert_eq!(
            mission.context_memory_extraction_refs,
            vec!["extract-a".to_string(), "extract-b".to_string()]
        );
        assert_eq!(
            mission.context_shared_context_ids,
            vec!["shared-a".to_string(), "shared-b".to_string()]
        );
        assert_eq!(
            mission.context_file_summary_paths,
            vec!["/tmp/a.rs".to_string(), "/tmp/b.rs".to_string()]
        );
        assert_eq!(mission.context_code_index_knowledge_count, 1);
        assert_eq!(mission.context_audited_knowledge_count, 1);
        assert_eq!(mission.context_governed_knowledge_count, 1);
        assert_eq!(mission.context_extracted_memory_count, 2);
        assert_eq!(mission.context_provenance_linked_memory_count, 3);
        assert_eq!(
            read_model
                .overview
                .diagnostics
                .context_execution_group_count,
            1
        );
        assert_eq!(
            read_model.overview.diagnostics.context_used_knowledge_count,
            2
        );
        assert_eq!(read_model.overview.diagnostics.context_used_memory_count, 3);
        assert_eq!(
            read_model
                .overview
                .diagnostics
                .context_code_index_knowledge_count,
            1
        );
        assert_eq!(
            read_model
                .overview
                .diagnostics
                .context_extracted_memory_count,
            2
        );
    }

    #[test]
    fn knowledge_context_events_remain_session_scoped_in_runtime_read_model() {
        let decisions = [
            "not_needed",
            "missing_workspace",
            "queried_no_match",
            "matched_not_injected",
            "injected",
        ];
        let mut events = decisions
            .iter()
            .enumerate()
            .map(|(index, decision)| {
                let mut event = EventEnvelope::audit(
                    EventId::new(format!("knowledge-event-{index}")),
                    "knowledge.context.selected",
                    json!({
                        "consumer": "mainline",
                        "decision": decision,
                        "knowledge_ids": [],
                        "result_kinds": [],
                        "matched_count": 0,
                        "injected_count": 0,
                        "injected_chars": 0,
                        "truncated": false
                    }),
                )
                .with_context(EventContext {
                    session_id: Some(SessionId::new("session-knowledge-audit")),
                    workspace_id: Some(WorkspaceId::new("workspace-knowledge-audit")),
                    ..EventContext::default()
                });
                event.sequence = index as u64 + 1;
                event
            })
            .collect::<Vec<_>>();
        let mut other_session_event = EventEnvelope::audit(
            EventId::new("knowledge-event-other"),
            "knowledge.context.selected",
            json!({ "consumer": "mainline", "decision": "injected" }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-knowledge-other")),
            workspace_id: Some(WorkspaceId::new("workspace-knowledge-audit")),
            ..EventContext::default()
        });
        other_session_event.sequence = 99;
        events.push(other_session_event);

        let read_model = RuntimeReadModelInput::from_events(&events);
        let session = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-knowledge-audit")
            .expect("knowledge audit session");

        assert_eq!(session.event_count, 5);
        assert_eq!(session.audit_event_count, 5);
        assert_eq!(
            session.latest_event_type.as_deref(),
            Some("knowledge.context.selected")
        );
        assert_eq!(read_model.details.knowledge_audit.len(), 6);
        let scoped_entries = read_model
            .details
            .knowledge_audit
            .iter()
            .filter(|entry| entry.session_id.as_deref() == Some("session-knowledge-audit"))
            .collect::<Vec<_>>();
        assert_eq!(scoped_entries.len(), 5);
        assert_eq!(scoped_entries[0].decision.as_deref(), Some("not_needed"));
        assert_eq!(scoped_entries[4].decision.as_deref(), Some("injected"));
    }

    #[test]
    fn mission_execution_overview_survives_followup_overview_without_context() {
        let mission_context = EventContext {
            mission_id: Some(MissionId::new("mission-1")),
            ..EventContext::default()
        };

        let mut mission_created = EventEnvelope::domain(
            EventId::new("event-1"),
            "mission.created",
            json!({
                "mission_id": "mission-1"
            }),
        )
        .with_context(mission_context.clone());
        mission_created.sequence = 1;

        let mut overview_with_context = EventEnvelope::audit(
            EventId::new("event-2"),
            "mission.execution.overview",
            json!({
                "mission_id": "mission-1",
                "total_tasks": 2,
                "completed_tasks": 0,
                "failed_tasks": 0,
                "context": {
                    "used_knowledge": 1,
                    "used_memory": 1,
                    "knowledge_source_paths": ["src/z.rs", "src/a.rs", "src/z.rs"],
                    "memory_extraction_refs": ["extract-z", "extract-a", "extract-z"],
                    "shared_context_ids": ["shared-z", "shared-a", "shared-z"],
                    "file_summary_paths": ["/tmp/z.rs", "/tmp/a.rs", "/tmp/z.rs"]
                }
            }),
        )
        .with_context(mission_context.clone());
        overview_with_context.sequence = 2;

        let mut followup_overview_without_context = EventEnvelope::audit(
            EventId::new("event-3"),
            "mission.execution.overview",
            json!({
                "mission_id": "mission-1",
                "total_tasks": 2,
                "completed_tasks": 1,
                "failed_tasks": 0
            }),
        )
        .with_context(mission_context);
        followup_overview_without_context.sequence = 3;

        let read_model = RuntimeReadModelInput::from_events(&[
            mission_created,
            overview_with_context,
            followup_overview_without_context,
        ]);
        let mission = read_model
            .details
            .execution_groups
            .first()
            .expect("execution group entry to exist");

        assert_eq!(mission.event_count, 3);
        assert_eq!(
            mission.latest_event_type.as_deref(),
            Some("mission.execution.overview")
        );
        assert_eq!(mission.current_status.as_deref(), Some("running"));
        assert_eq!(
            mission.context_knowledge_source_paths,
            vec!["src/a.rs".to_string(), "src/z.rs".to_string()]
        );
        assert_eq!(
            mission.context_memory_extraction_refs,
            vec!["extract-a".to_string(), "extract-z".to_string()]
        );
        assert_eq!(
            mission.context_shared_context_ids,
            vec!["shared-a".to_string(), "shared-z".to_string()]
        );
        assert_eq!(
            mission.context_file_summary_paths,
            vec!["/tmp/a.rs".to_string(), "/tmp/z.rs".to_string()]
        );
        assert_eq!(
            read_model
                .overview
                .diagnostics
                .context_execution_group_count,
            1
        );
    }

    #[test]
    fn task_status_changed_updates_runtime_task_status_to_terminal() {
        let mut task_dispatched = EventEnvelope::domain(
            EventId::new("event-task-dispatched"),
            "task.dispatched",
            json!({}),
        )
        .with_context(EventContext {
            mission_id: Some(MissionId::new("mission-1")),
            task_id: Some(TaskId::new("task-1")),
            session_id: Some(SessionId::new("session-1")),
            ..EventContext::default()
        });
        task_dispatched.sequence = 1;

        let mut task_completed = crate::task_events::task_status_changed_event(
            "task-1",
            "mission-1",
            "Running",
            "Completed",
            "Action",
        );
        task_completed.sequence = 2;

        let read_model = RuntimeReadModelInput::from_events(&[task_dispatched, task_completed]);
        let task = read_model
            .details
            .tasks
            .iter()
            .find(|entry| entry.task_id == "task-1")
            .expect("task runtime entry should exist");
        assert_eq!(task.current_status.as_deref(), Some("completed"));

        let mission = read_model
            .details
            .execution_groups
            .iter()
            .find(|entry| entry.mission_id == "mission-1")
            .expect("mission runtime entry should exist");
        assert_eq!(mission.current_status.as_deref(), Some("succeeded"));
    }

    #[test]
    fn model_usage_recorded_records_session_usage_observation() {
        let mut usage_event = EventEnvelope::audit(
            EventId::new("event-model-usage-recorded"),
            "model.usage.recorded",
            json!({
                "status": "success",
                "usage": {
                    "inputTokens": 12_000,
                    "outputTokens": 3_000,
                    "cacheReadTokens": 2_000,
                    "cacheReadIncludedInInput": true
                },
                "executionBinding": { "role": "orchestrator" },
                "modelConfig": { "model": "gpt-5-codex" },
                "timestamp": 1_700_000_000_000_u64
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-usage")),
            ..EventContext::default()
        });
        usage_event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[usage_event]);
        let session = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-usage")
            .expect("session runtime entry should exist");
        let observation = session
            .usage_observation
            .as_ref()
            .expect("usage observation should be recorded");

        // 上下文窗口占用按 raw input + output,cache read 仍占窗口,不能按费用口径扣减。
        assert_eq!(observation.context_window_tokens, 15_000);
        assert_eq!(observation.resolved_model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(observation.observed_at, Some(UtcMillis(1_700_000_000_000)));
        // event-bus 不计算窗口/告警,budget 留给 magi-api 装配。
        assert!(session.budget.is_none());
    }

    #[test]
    fn live_context_usage_event_updates_session_observation_before_turn_completion() {
        let mut context_event = EventEnvelope::domain(
            EventId::new("event-live-context-usage"),
            "session.context.usage.updated",
            json!({
                "token_used": 24_000,
                "resolved_model": "gpt-5-codex",
                "accuracy": "estimated",
                "phase": "streaming",
                "updated_at": 1_700_000_000_100_u64
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-live-context")),
            ..EventContext::default()
        });
        context_event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[context_event]);
        let observation = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-live-context")
            .and_then(|entry| entry.usage_observation.as_ref())
            .expect("live context observation should exist");
        assert_eq!(observation.context_window_tokens, 24_000);
        assert_eq!(observation.resolved_model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(observation.observed_at, Some(UtcMillis(1_700_000_000_100)));
        assert_eq!(observation.measurement.as_deref(), Some("estimated"));
        assert_eq!(observation.phase.as_deref(), Some("streaming"));
    }

    #[test]
    fn model_usage_recorded_prefers_total_tokens_for_context_window() {
        let mut usage_event = EventEnvelope::audit(
            EventId::new("event-model-usage-total-tokens"),
            "model.usage.recorded",
            json!({
                "status": "success",
                "usage": {
                    "inputTokens": 12_000,
                    "outputTokens": 3_000,
                    "totalTokens": 18_000
                },
                "executionBinding": { "role": "orchestrator" },
                "modelConfig": { "model": "gpt-5-codex" },
                "timestamp": 1_700_000_000_000_u64
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-usage-total")),
            ..EventContext::default()
        });
        usage_event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[usage_event]);
        let observation = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-usage-total")
            .and_then(|entry| entry.usage_observation.as_ref())
            .expect("usage observation should be recorded");

        assert_eq!(observation.context_window_tokens, 18_000);
    }

    #[test]
    fn session_context_compacted_records_latest_session_compaction() {
        let mut compacted_event = EventEnvelope::domain(
            EventId::new("event-session-context-compacted"),
            "session.context.compacted",
            json!({
                "reason": "context_window_pressure",
                "phase": "turn_start",
                "original_message_count": 42,
                "compacted_message_count": 9,
                "original_token_estimate": 180_000,
                "compacted_token_estimate": 36_000,
                "context_window_tokens": 245_000,
                "token_limit": 272_000,
                "threshold_tokens": 244_800,
                "resolved_model": "gpt-5-codex",
                "compacted_at": 1_700_000_000_002_u64
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-compacted")),
            ..EventContext::default()
        });
        compacted_event.sequence = 1;

        let read_model = RuntimeReadModelInput::from_events(&[compacted_event]);
        let compaction = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-compacted")
            .and_then(|entry| entry.context_compaction.as_ref())
            .expect("session compaction summary should be recorded");

        assert_eq!(compaction.reason, "context_window_pressure");
        assert_eq!(compaction.phase.as_deref(), Some("turn_start"));
        assert_eq!(compaction.original_message_count, 42);
        assert_eq!(compaction.compacted_message_count, 9);
        assert_eq!(compaction.original_token_estimate, 180_000);
        assert_eq!(compaction.compacted_token_estimate, 36_000);
        assert_eq!(compaction.context_window_tokens, Some(245_000));
        assert_eq!(compaction.token_limit, Some(272_000));
        assert_eq!(compaction.threshold_tokens, Some(244_800));
        assert_eq!(compaction.resolved_model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(compaction.compacted_at, Some(UtcMillis(1_700_000_000_002)));
    }

    #[test]
    fn model_usage_recorded_ignores_worker_usage_for_session_context_ring() {
        let mut orchestrator_event = EventEnvelope::audit(
            EventId::new("event-orchestrator-usage"),
            "model.usage.recorded",
            json!({
                "status": "success",
                "usage": {
                    "inputTokens": 20_000,
                    "outputTokens": 1_000
                },
                "executionBinding": { "role": "orchestrator" },
                "modelConfig": { "model": "gpt-5-codex" },
                "timestamp": 700_u64
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-role-filter")),
            ..EventContext::default()
        });
        orchestrator_event.sequence = 1;

        let mut worker_event = EventEnvelope::audit(
            EventId::new("event-worker-usage"),
            "model.usage.recorded",
            json!({
                "status": "success",
                "usage": {
                    "inputTokens": 99_000,
                    "outputTokens": 9_000
                },
                "executionBinding": { "role": "worker" },
                "modelConfig": { "model": "gpt-5-codex" },
                "timestamp": 900_u64
            }),
        )
        .with_context(EventContext {
            session_id: Some(SessionId::new("session-role-filter")),
            ..EventContext::default()
        });
        worker_event.sequence = 2;

        let read_model = RuntimeReadModelInput::from_events(&[orchestrator_event, worker_event]);
        let observation = read_model
            .details
            .sessions
            .iter()
            .find(|entry| entry.session_id == "session-role-filter")
            .and_then(|entry| entry.usage_observation.as_ref())
            .expect("orchestrator usage observation should be retained");

        assert_eq!(observation.context_window_tokens, 21_000);
        assert_eq!(observation.observed_at, Some(UtcMillis(700)));
    }

    fn usage_ledger_entry(
        session_id: &str,
        sequence: u64,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
        timestamp: u64,
    ) -> crate::AuditUsageLedgerEntry {
        crate::AuditUsageLedgerEntry {
            event_id: format!("event-{session_id}-{sequence}"),
            event_type: "model.usage.recorded".to_string(),
            occurred_at: UtcMillis(timestamp),
            sequence,
            context: EventContext {
                session_id: Some(SessionId::new(session_id)),
                ..EventContext::default()
            },
            payload: json!({
                "status": "success",
                "usage": {
                    "inputTokens": input_tokens,
                    "outputTokens": output_tokens
                },
                "executionBinding": { "role": "orchestrator" },
                "modelConfig": { "model": model },
                "timestamp": timestamp
            }),
        }
    }

    #[test]
    fn latest_usage_observations_from_ledger_keeps_latest_observed_usage_per_session() {
        // 同一会话两条用量条目,乱序输入;应保留 timestamp 更新的那条。
        // 另含一条无 session 上下文与一条非 usage 事件,均应被忽略。
        let mut noise = EventEnvelope::audit(
            EventId::new("event-non-usage"),
            "session.activity.recorded",
            json!({ "status": "success" }),
        );
        noise.sequence = 99;
        let noise_entry = crate::AuditUsageLedgerEntry {
            event_id: "event-non-usage".to_string(),
            event_type: "session.activity.recorded".to_string(),
            occurred_at: UtcMillis(1_000),
            sequence: 99,
            context: EventContext {
                session_id: Some(SessionId::new("session-a")),
                ..EventContext::default()
            },
            payload: json!({ "status": "success" }),
        };
        let mut orphan = usage_ledger_entry("session-b", 5, 1_000, 0, "gpt-5-codex", 10);
        orphan.context.session_id = None;

        let entries = vec![
            usage_ledger_entry("session-a", 7, 20_000, 1_000, "gpt-5-codex", 700),
            usage_ledger_entry("session-a", 3, 5_000, 500, "gpt-5-codex", 300),
            noise_entry,
            orphan,
        ];

        let observations = latest_usage_observations_from_ledger(&entries);

        assert_eq!(observations.len(), 1);
        let observation = observations
            .get("session-a")
            .expect("session-a observation should be present");
        // timestamp 700 wins:20_000 input + 1_000 output.
        assert_eq!(observation.context_window_tokens, 21_000);
        assert_eq!(observation.observed_at, Some(UtcMillis(700)));
    }

    #[test]
    fn latest_usage_observations_from_ledger_survives_sequence_reset_after_restart() {
        let entries = vec![
            usage_ledger_entry("session-a", 5_216, 6_000, 480, "gpt-5.5", 1_782_654_948_746),
            usage_ledger_entry("session-a", 93, 7_000, 72, "gpt-5.5", 1_783_502_851_661),
        ];

        let observations = latest_usage_observations_from_ledger(&entries);
        let observation = observations
            .get("session-a")
            .expect("session-a observation should be present");

        assert_eq!(observation.context_window_tokens, 7_072);
        assert_eq!(observation.observed_at, Some(UtcMillis(1_783_502_851_661)));
    }

    #[test]
    fn latest_usage_observations_from_ledger_ignores_newer_worker_usage() {
        let mut worker_entry =
            usage_ledger_entry("session-a", 8, 99_000, 1_000, "gpt-5-codex", 800);
        worker_entry.payload["executionBinding"]["role"] = json!("worker");

        let entries = vec![
            usage_ledger_entry("session-a", 7, 20_000, 1_000, "gpt-5-codex", 700),
            worker_entry,
        ];

        let observations = latest_usage_observations_from_ledger(&entries);
        let observation = observations
            .get("session-a")
            .expect("orchestrator observation should be present");

        assert_eq!(observation.context_window_tokens, 21_000);
        assert_eq!(observation.observed_at, Some(UtcMillis(700)));
    }
}
