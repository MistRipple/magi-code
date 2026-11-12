//! 把事件流折叠成运行态读模型：逐事件更新各实体的累加表，最后装配 meta / overview / details / operations / recovery。

use super::*;
use serde_json::Value;

/// 知识审计条目只保留最近这么多条。
const KNOWLEDGE_AUDIT_LIMIT: usize = 64;

/// 单个事件解析出的任务域归属，供各实体累加共用。
struct EventScope {
    mission_id: Option<String>,
    task_id: Option<String>,
}

impl EventScope {
    fn of(event: &EventEnvelope) -> Self {
        Self {
            mission_id: event_mission_id(event),
            task_id: event_task_id(event),
        }
    }
}

fn is_worker_event(event: &EventEnvelope) -> bool {
    event.event_type.starts_with("worker.")
}

fn is_tool_event(event: &EventEnvelope) -> bool {
    event.event_type.starts_with("tool.")
}

fn is_resume_event(event: &EventEnvelope) -> bool {
    event.event_type.contains(".resume.") || event.event_type.contains(".resumed.")
}

fn is_dispatch_event(event: &EventEnvelope) -> bool {
    event.event_type == "task.dispatched" || event.event_type == "mission.resume.dispatch.created"
}

fn payload_string(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_string)
}

fn payload_lowercase(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
}

fn push_unique(target: &mut Vec<String>, value: &str) {
    if !target.iter().any(|existing| existing == value) {
        target.push(value.to_string());
    }
}

fn push_unique_opt(target: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        push_unique(target, value);
    }
}

/// 在实体条目上累加事件总数、审计数，并记录最近事件类型。
macro_rules! count_event {
    ($entry:expr, $event:expr) => {{
        let entry = &mut *$entry;
        entry.event_count += 1;
        if $event.category == EventCategory::Audit {
            entry.audit_event_count += 1;
        }
        entry.latest_event_type = Some($event.event_type.clone());
    }};
}

/// 会话与工作区条目共有的 worker / tool / 恢复事件计数。
macro_rules! count_event_kinds {
    ($entry:expr, $event:expr) => {{
        let entry = &mut *$entry;
        if is_worker_event($event) {
            entry.worker_event_count += 1;
        }
        if is_tool_event($event) {
            entry.tool_event_count += 1;
        }
        if is_resume_event($event) {
            entry.recovery_event_count += 1;
        }
    }};
}

/// `worker.skill_dispatch.*` 事件在任务域、任务和 worker 条目上的同一套累加。
macro_rules! count_skill_dispatch {
    ($entry:expr, $event:expr) => {{
        let entry = &mut *$entry;
        entry.skill_dispatch_count += 1;
        match payload_lowercase(&$event.payload, "route").as_deref() {
            Some("builtin") => entry.builtin_dispatch_count += 1,
            Some("bridge") => entry.bridge_dispatch_count += 1,
            _ => {}
        }
        match payload_lowercase(&$event.payload, "status").as_deref() {
            Some("rejected") => entry.rejected_dispatch_count += 1,
            Some("failed") => entry.failed_dispatch_count += 1,
            _ => {}
        }
    }};
}

/// 治理决策计数与涉及的任务 / worker。
#[derive(Default)]
struct GovernanceTally {
    total: usize,
    allowed: usize,
    needs_approval: usize,
    blocked: usize,
    rejected: usize,
    blocked_task_ids: Vec<String>,
    approval_required_task_ids: Vec<String>,
    rejected_task_ids: Vec<String>,
    blocked_worker_ids: Vec<String>,
    approval_required_worker_ids: Vec<String>,
    rejected_worker_ids: Vec<String>,
}

impl GovernanceTally {
    fn observe(&mut self, event: &EventEnvelope) {
        if event.event_type != "governance.decision.applied" {
            return;
        }
        self.total += 1;
        match payload_lowercase(&event.payload, "status").as_deref() {
            Some("allowed") => self.allowed += 1,
            Some("needs_approval") => self.needs_approval += 1,
            Some("blocked") => self.blocked += 1,
            Some("rejected") => self.rejected += 1,
            _ => {}
        }
        for task_ids in [
            &mut self.blocked_task_ids,
            &mut self.approval_required_task_ids,
            &mut self.rejected_task_ids,
        ] {
            collect_unique_payload_value(task_ids, &event.payload, "task_id");
        }
        for worker_ids in [
            &mut self.blocked_worker_ids,
            &mut self.approval_required_worker_ids,
            &mut self.rejected_worker_ids,
        ] {
            collect_unique_payload_value(worker_ids, &event.payload, "worker_id");
        }
    }

    fn diagnostic_counts(&self) -> GovernanceDiagnosticCounts {
        GovernanceDiagnosticCounts {
            total: self.total,
            allowed: self.allowed,
            needs_approval: self.needs_approval,
            blocked: self.blocked,
            rejected: self.rejected,
        }
    }

    fn attention_ids(&self) -> GovernanceAttentionIds<'_> {
        GovernanceAttentionIds {
            blocked_task_ids: &self.blocked_task_ids,
            approval_required_task_ids: &self.approval_required_task_ids,
            rejected_task_ids: &self.rejected_task_ids,
            blocked_worker_ids: &self.blocked_worker_ids,
            approval_required_worker_ids: &self.approval_required_worker_ids,
            rejected_worker_ids: &self.rejected_worker_ids,
        }
    }

    /// 治理数据以事件流直接统计的结果为准，覆盖到最终读模型上。
    fn apply_to(self, read_model: &mut RuntimeReadModelInput) {
        let diagnostics = &mut read_model.overview.diagnostics;
        diagnostics.governance_total_count = self.total;
        diagnostics.governance_allowed_count = self.allowed;
        diagnostics.governance_needs_approval_count = self.needs_approval;
        diagnostics.governance_blocked_count = self.blocked;
        diagnostics.governance_rejected_count = self.rejected;
        let attention = &mut read_model.operations.attention;
        attention.governance_blocked_task_ids = self.blocked_task_ids;
        attention.governance_approval_required_task_ids = self.approval_required_task_ids;
        attention.governance_rejected_task_ids = self.rejected_task_ids;
        attention.governance_blocked_worker_ids = self.blocked_worker_ids;
        attention.governance_approval_required_worker_ids = self.approval_required_worker_ids;
        attention.governance_rejected_worker_ids = self.rejected_worker_ids;
    }
}

/// 逐事件累加的中间状态。
#[derive(Default)]
struct EventFold {
    category_counts: EventCategoryCounts,
    summary: RuntimeActivitySummary,
    execution_groups: BTreeMap<String, ExecutionGroupRuntimeSummaryEntry>,
    mission_progress: BTreeMap<String, MissionProgressSummary>,
    tasks: BTreeMap<String, TaskRuntimeSummaryEntry>,
    assignments: BTreeMap<String, AssignmentRuntimeSummaryEntry>,
    workers: BTreeMap<String, WorkerRuntimeSummaryEntry>,
    tools: BTreeMap<String, ToolRuntimeSummaryEntry>,
    sessions: BTreeMap<String, SessionRuntimeSummaryEntry>,
    workspaces: BTreeMap<String, WorkspaceRuntimeSummaryEntry>,
    knowledge_audit: Vec<KnowledgeAuditRuntimeEntry>,
    dispatch: DispatchRuntimeSummary,
    governance: GovernanceTally,
}

impl EventFold {
    fn observe(&mut self, event: &EventEnvelope) {
        if let Some(entry) = KnowledgeAuditRuntimeEntry::from_event(event) {
            self.knowledge_audit.push(entry);
        }
        let scope = EventScope::of(event);
        self.count_activity(event, &scope);
        self.observe_session(event, &scope);
        self.observe_workspace(event, &scope);
        self.observe_mission(event, &scope);
        self.observe_task(event, &scope);
        self.observe_assignment(event, &scope);
        self.observe_worker(event);
        self.observe_dispatch(event);
        self.observe_tool(event);
        self.governance.observe(event);
    }

    fn count_activity(&mut self, event: &EventEnvelope, scope: &EventScope) {
        match event.category {
            EventCategory::Domain => self.category_counts.domain += 1,
            EventCategory::Audit => self.category_counts.audit += 1,
            EventCategory::Usage => self.category_counts.usage += 1,
            EventCategory::Projection => self.category_counts.projection += 1,
            EventCategory::System => self.category_counts.system += 1,
        }
        let summary = &mut self.summary;
        if scope.mission_id.is_some() {
            summary.execution_group_event_count += 1;
        }
        if is_worker_event(event) {
            summary.worker_event_count += 1;
        }
        if event.event_type == "worker.skill_dispatch.observed" {
            summary.skill_dispatch_event_count += 1;
        }
        if event.event_type == "worker.executor.observed" {
            summary.executor_event_count += 1;
        }
        if is_tool_event(event) {
            summary.tool_event_count += 1;
        }
        if is_resume_event(event) {
            summary.recovery_event_count += 1;
        }
    }

    fn observe_session(&mut self, event: &EventEnvelope, scope: &EventScope) {
        let Some(session_id) = event.session_id.as_ref() else {
            return;
        };
        let session_id = session_id.to_string();
        let entry =
            self.sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionRuntimeSummaryEntry {
                    session_id,
                    ..SessionRuntimeSummaryEntry::default()
                });
        count_event!(entry, event);
        count_event_kinds!(entry, event);
        if let Some(observation) = usage_observation_from_event(event) {
            entry.usage_observation = Some(observation);
        }
        if let Some(compaction) = context_compaction_from_event(event) {
            entry.context_compaction = Some(compaction);
        }
        push_unique_opt(
            &mut entry.active_execution_group_ids,
            scope.mission_id.as_deref(),
        );
        push_unique_opt(&mut entry.active_task_ids, scope.task_id.as_deref());
        collect_unique_payload_value(&mut entry.recovery_ids, &event.payload, "recovery_id");
    }

    fn observe_workspace(&mut self, event: &EventEnvelope, scope: &EventScope) {
        let Some(workspace_id) = event.workspace_id.as_ref() else {
            return;
        };
        let workspace_id = workspace_id.to_string();
        let entry = self
            .workspaces
            .entry(workspace_id.clone())
            .or_insert_with(|| WorkspaceRuntimeSummaryEntry {
                workspace_id,
                ..WorkspaceRuntimeSummaryEntry::default()
            });
        count_event!(entry, event);
        count_event_kinds!(entry, event);
        push_unique_opt(
            &mut entry.active_execution_group_ids,
            scope.mission_id.as_deref(),
        );
        push_unique_opt(&mut entry.active_task_ids, scope.task_id.as_deref());
        collect_unique_payload_value(&mut entry.recovery_ids, &event.payload, "recovery_id");
        collect_unique_payload_value(
            &mut entry.execution_chain_refs,
            &event.payload,
            "execution_chain_ref",
        );
    }

    fn observe_mission(&mut self, event: &EventEnvelope, scope: &EventScope) {
        let Some(mission_id) = scope.mission_id.as_ref() else {
            return;
        };
        push_unique(&mut self.summary.active_execution_group_ids, mission_id);
        let entry = self
            .execution_groups
            .entry(mission_id.clone())
            .or_insert_with(|| ExecutionGroupRuntimeSummaryEntry {
                mission_id: mission_id.clone(),
                ..ExecutionGroupRuntimeSummaryEntry::default()
            });
        entry.event_count += 1;
        if event.category == EventCategory::Audit {
            entry.audit_event_count += 1;
        }
        if event.event_type == "worker.skill_dispatch.applied" {
            count_skill_dispatch!(entry, event);
        }
        if event.event_type == "mission.execution.overview" {
            if let Some(progress) = mission_progress_from_payload(&event.payload) {
                self.mission_progress.insert(mission_id.clone(), progress);
            }
            if let Some(context) = event.payload.get("context") {
                apply_mission_context(entry, context);
            }
        }
        entry.latest_event_type = Some(event.event_type.clone());
        push_unique_opt(&mut entry.active_task_ids, scope.task_id.as_deref());
    }

    fn observe_task(&mut self, event: &EventEnvelope, scope: &EventScope) {
        let Some(task_id) = scope.task_id.as_ref() else {
            return;
        };
        push_unique(&mut self.summary.active_task_ids, task_id);
        let entry = self
            .tasks
            .entry(task_id.clone())
            .or_insert_with(|| TaskRuntimeSummaryEntry {
                task_id: task_id.clone(),
                mission_id: scope.mission_id.clone(),
                assignment_id: event.assignment_id.as_ref().map(ToString::to_string),
                ..TaskRuntimeSummaryEntry::default()
            });
        count_event!(entry, event);
        if event.event_type == "worker.skill_dispatch.applied" {
            count_skill_dispatch!(entry, event);
        }
        if let Some(status) = infer_task_status(event) {
            if event.event_type == "task.status.changed" {
                entry.failure_detail = if matches!(status.as_str(), "failed" | "blocked") {
                    event
                        .payload
                        .get("failure_detail")
                        .and_then(Value::as_str)
                        .map(|value| public_runtime_excerpt(value, 4096))
                        .filter(|value| !value.is_empty())
                } else {
                    None
                };
            }
            entry.current_status = Some(status);
        }
    }

    fn observe_assignment(&mut self, event: &EventEnvelope, scope: &EventScope) {
        let Some(assignment_id) = event.assignment_id.as_ref() else {
            return;
        };
        let assignment_id = assignment_id.to_string();
        let entry = self
            .assignments
            .entry(assignment_id.clone())
            .or_insert_with(|| AssignmentRuntimeSummaryEntry {
                assignment_id,
                mission_id: event.mission_id.as_ref().map(ToString::to_string),
                ..AssignmentRuntimeSummaryEntry::default()
            });
        count_event!(entry, event);
        push_unique_opt(&mut entry.task_ids, scope.task_id.as_deref());
        if is_dispatch_event(event) {
            entry.dispatch_count += 1;
        }
        if let Some(status) = infer_assignment_status(event) {
            entry.current_status = Some(status);
        }
        let report_status = event.payload.get("status").and_then(Value::as_str);
        if matches!(
            event.event_type.as_str(),
            "task.completed" | "worker.report.applied"
        ) && report_status == Some("Succeeded")
        {
            entry.completed_task_count += 1;
        }
        if matches!(
            event.event_type.as_str(),
            "task.failed" | "worker.report.applied"
        ) && matches!(report_status, Some("Failed") | Some("Blocked"))
        {
            entry.failed_task_count += 1;
        }
    }

    fn observe_worker(&mut self, event: &EventEnvelope) {
        let Some(worker_id) = payload_string(&event.payload, "worker_id") else {
            return;
        };
        let entry =
            self.workers
                .entry(worker_id.clone())
                .or_insert_with(|| WorkerRuntimeSummaryEntry {
                    worker_id,
                    ..WorkerRuntimeSummaryEntry::default()
                });
        count_event!(entry, event);
        if event.event_type == "worker.reported" {
            entry.report_count += 1;
        }
        if event.event_type == "worker.tool.observed" || event.event_type == "tool.invoked" {
            entry.tool_call_count += 1;
        }
        if event.event_type == "worker.skill_dispatch.observed" {
            count_skill_dispatch!(entry, event);
        }
        if event.event_type == "worker.executor.observed" {
            apply_executor_observation(entry, &event.payload);
        }
        if let Some(task_id) = payload_string(&event.payload, "task_id") {
            entry.current_task_id = Some(task_id);
        } else if let Some(task_id) = event.task_id.as_ref() {
            entry.current_task_id = Some(task_id.to_string());
        }
        if let Some(status) = infer_worker_status(event) {
            entry.current_status = Some(status);
        }
        if let Some(stage) = infer_worker_stage(event) {
            entry.current_stage = Some(stage);
        }
    }

    fn observe_dispatch(&mut self, event: &EventEnvelope) {
        if !is_dispatch_event(event) {
            return;
        }
        let dispatch = &mut self.dispatch;
        dispatch.total_dispatches += 1;
        if let Some(assignment_id) = event.assignment_id.as_ref() {
            push_unique(
                &mut dispatch.active_assignment_ids,
                &assignment_id.to_string(),
            );
        }
        if event.event_type == "mission.resume.dispatch.created" {
            dispatch.resume_dispatches += 1;
        }
        if let Some(reason) = payload_lowercase(&event.payload, "dispatch_reason") {
            dispatch.latest_dispatch_reason = Some(reason);
        }
    }

    fn observe_tool(&mut self, event: &EventEnvelope) {
        if event.event_type != "tool.invoked" {
            return;
        }
        let tool_name = event
            .payload
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let entry =
            self.tools
                .entry(tool_name.clone())
                .or_insert_with(|| ToolRuntimeSummaryEntry {
                    tool_name,
                    ..ToolRuntimeSummaryEntry::default()
                });
        entry.event_count += 1;
        entry.latest_event_type = Some(event.event_type.clone());
        entry.tool_kind = payload_string(&event.payload, "tool_kind");
        if let Some(status) = payload_lowercase(&event.payload, "status") {
            match status.as_str() {
                "succeeded" => entry.success_count += 1,
                "needsapproval" | "rejected" => entry.blocked_count += 1,
                "failed" => entry.failed_count += 1,
                _ => {}
            }
            entry.latest_status = Some(status);
        }
        collect_unique_payload_value(&mut entry.worker_ids, &event.payload, "worker_id");
        collect_unique_payload_value(&mut entry.task_ids, &event.payload, "task_id");
        collect_unique_payload_value(&mut entry.session_ids, &event.payload, "session_id");
        collect_unique_payload_value(&mut entry.workspace_ids, &event.payload, "workspace_id");
    }
}

/// `mission.execution.overview` 事件携带的上下文用量，整体替换任务域条目里的对应字段。
fn apply_mission_context(entry: &mut ExecutionGroupRuntimeSummaryEntry, context: &Value) {
    entry.context_used_turn_count = nested_usize_field(context, "used_turns");
    entry.context_used_knowledge_count = nested_usize_field(context, "used_knowledge");
    entry.context_used_memory_count = nested_usize_field(context, "used_memory");
    entry.context_used_shared_item_count = nested_usize_field(context, "used_shared_items");
    entry.context_used_file_summary_count = nested_usize_field(context, "used_file_summaries");
    entry.context_recent_turn_resolved_count =
        nested_usize_field(context, "recent_turn_resolved_count");
    entry.context_recent_turn_retained_count =
        nested_usize_field(context, "recent_turn_retained_count");
    entry.context_recent_turn_session_source_count =
        nested_usize_field(context, "recent_turn_session_source_count");
    entry.context_recent_turn_project_source_count =
        nested_usize_field(context, "recent_turn_project_source_count");
    entry.context_recent_turn_provided_source_count =
        nested_usize_field(context, "recent_turn_provided_source_count");
    entry.context_truncation_count = nested_usize_field(context, "truncation_count");
    entry.context_truncation_parts = nested_string_vec_field(context, "truncation_parts");
    entry.context_knowledge_ids = nested_string_vec_field(context, "knowledge_ids");
    entry.context_knowledge_source_paths =
        nested_string_vec_field(context, "knowledge_source_paths");
    entry.context_memory_ids = nested_string_vec_field(context, "memory_ids");
    entry.context_memory_extraction_refs =
        nested_string_vec_field(context, "memory_extraction_refs");
    entry.context_shared_context_ids = nested_string_vec_field(context, "shared_context_ids");
    entry.context_file_summary_paths = nested_string_vec_field(context, "file_summary_paths");
    entry.context_code_index_knowledge_count =
        nested_usize_field(context, "code_index_knowledge_count");
    entry.context_audited_knowledge_count = nested_usize_field(context, "audited_knowledge_count");
    entry.context_governed_knowledge_count =
        nested_usize_field(context, "governed_knowledge_count");
    entry.context_extracted_memory_count = nested_usize_field(context, "extracted_memory_count");
    entry.context_provenance_linked_memory_count =
        nested_usize_field(context, "provenance_linked_memory_count");
}

/// `worker.executor.observed` 事件整体替换 worker 条目里的 executor 观测字段。
fn apply_executor_observation(entry: &mut WorkerRuntimeSummaryEntry, payload: &Value) {
    entry.executor_kind = payload_string(payload, "executor_kind");
    entry.executor_id = payload_string(payload, "executor_id");
    entry.executor_version = payload_string(payload, "executor_version");
    entry.executor_instance_id = payload_string(payload, "executor_instance_id");
    entry.executor_lease_id = payload_string(payload, "executor_lease_id");
    entry.executor_request_id = payload_string(payload, "request_id");
    entry.executor_request_source = payload_string(payload, "request_source");
    entry.executor_observation_status = payload_string(payload, "observation_status");
    entry.executor_requested_reuse_policy = payload_string(payload, "requested_reuse_policy");
    entry.executor_requested_binding_scope = payload_string(payload, "requested_binding_scope");
    entry.executor_requested_lease_state = payload_string(payload, "requested_lease_state");
    entry.executor_requested_binding_lifecycle =
        payload_string(payload, "requested_binding_lifecycle");
    entry.executor_requested_process_lifecycle =
        payload_string(payload, "requested_process_lifecycle");
    entry.executor_requested_process_model = payload_string(payload, "requested_process_model");
    entry.executor_requested_parallelism = payload
        .get("requested_parallelism")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    entry.executor_requested_step_kinds = nested_string_vec_field(payload, "requested_step_kinds");
    entry.executor_execution_mode = payload_string(payload, "execution_mode");
    entry.executor_process_model = payload_string(payload, "process_model");
    entry.executor_lease_state = payload_string(payload, "lease_state");
    entry.executor_binding_lifecycle = payload_string(payload, "binding_lifecycle");
    entry.executor_process_lifecycle = payload_string(payload, "process_lifecycle");
    entry.executor_reuse_scope = payload_string(payload, "reuse_scope");
    entry.executor_parallelism_scope = payload_string(payload, "parallelism_scope");
    entry.executor_health_status = payload_lowercase(payload, "health_status");
    entry.executor_failure_layer = payload_string(payload, "failure_layer");
    entry.executor_failure_message = payload
        .get("failure_message")
        .and_then(Value::as_str)
        .and_then(|value| public_runtime_summary(Some(value)));
    entry.executor_supported_step_kinds = nested_string_vec_field(payload, "supported_step_kinds");
}

fn build_meta(
    events: &[EventEnvelope],
    category_counts: &EventCategoryCounts,
    latest_sequence: u64,
) -> RuntimeMetaSummary {
    RuntimeMetaSummary {
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
        recent_event_count: events.len(),
    }
}

impl RuntimeReadModelInput {
    pub fn from_events(events: &[EventEnvelope]) -> Self {
        let latest_sequence = events.iter().map(|event| event.sequence).max().unwrap_or(0);
        let mut fold = EventFold::default();
        for event in events {
            fold.observe(event);
        }
        let EventFold {
            category_counts,
            summary,
            mut execution_groups,
            mission_progress,
            tasks,
            assignments,
            workers,
            tools,
            sessions,
            workspaces,
            mut knowledge_audit,
            dispatch,
            governance,
        } = fold;

        let recovery = RecoveryReadModelInput::from_events(events);
        refresh_execution_group_statuses(
            &mut execution_groups,
            &tasks,
            &recovery,
            &mission_progress,
        );

        let execution_groups = execution_groups.into_values().collect::<Vec<_>>();
        let tasks = tasks.into_values().collect::<Vec<_>>();
        let assignments = assignments.into_values().collect::<Vec<_>>();
        let workers = workers.into_values().collect::<Vec<_>>();
        let tools = tools.into_values().collect::<Vec<_>>();
        let sessions = sessions.into_values().collect::<Vec<_>>();
        let workspaces = workspaces.into_values().collect::<Vec<_>>();
        knowledge_audit.sort_by_key(|entry| entry.sequence);
        if knowledge_audit.len() > KNOWLEDGE_AUDIT_LIMIT {
            knowledge_audit.drain(..knowledge_audit.len() - KNOWLEDGE_AUDIT_LIMIT);
        }

        let components = RuntimeAggregateComponents {
            execution_groups: &execution_groups,
            tasks: &tasks,
            assignments: &assignments,
            workers: &workers,
            tools: &tools,
            recovery: &recovery,
        };
        let diagnostics =
            RuntimeDiagnosticSummary::from_components(&components, governance.diagnostic_counts());
        let attention =
            RuntimeAttentionSummary::from_components(&components, governance.attention_ids());
        let work_queues = RuntimeWorkQueueSummary::from_components(
            &execution_groups,
            &tasks,
            &assignments,
            &workers,
            &recovery,
        );
        let operations = RuntimeOperationsSummary {
            dispatch,
            attention,
            work_queues,
            resume_observation: RecoveryResumeObservationSummary::from_recovery(&recovery),
        };
        let meta = build_meta(events, &category_counts, latest_sequence);
        let overview = RuntimeOverviewSummary {
            category_counts,
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

        let mut read_model = RuntimeReadModelInput {
            meta,
            overview,
            details,
            operations,
            recovery,
        }
        .normalize();
        read_model.refresh_contract_state();
        governance.apply_to(&mut read_model);
        read_model
    }
}
