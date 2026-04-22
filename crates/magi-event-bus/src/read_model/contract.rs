use super::*;

pub const RUNTIME_READ_MODEL_CONTRACT_VERSION: &str = "shadow-runtime-v1";
pub const RUNTIME_READ_MODEL_CONTRACT_SECTIONS: [&str; 5] =
    ["meta", "overview", "details", "operations", "recovery"];
pub const RUNTIME_READ_MODEL_ORDERING_STRATEGY: &str = "deterministic-lexicographic";
pub const RUNTIME_READ_MODEL_SECTION_ORDERING_RULES: [(&str, &str); 10] = [
    ("details.execution_groups", "mission_id asc"),
    ("details.tasks", "task_id asc"),
    ("details.assignments", "assignment_id asc"),
    ("details.workers", "worker_id asc"),
    ("details.tools", "tool_name asc"),
    ("details.sessions", "session_id asc"),
    ("details.workspaces", "workspace_id asc"),
    ("recovery.entries", "sequence asc"),
    ("recovery.summaries", "recovery_id asc"),
    ("nested.string_sets", "lexicographic asc + unique"),
];
pub const RUNTIME_READ_MODEL_REQUIRED_VALIDATION_REFS: [&str; 6] = [
    "runtime_read_model.contract_version",
    "runtime_read_model.contract_sections",
    "runtime_read_model.ordering_strategy",
    "runtime_read_model.section_ordering_rules",
    "runtime_read_model.freeze_signature",
    "runtime_read_model.validation_passed",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeSectionOrderingRule {
    pub target: String,
    pub ordering: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractValidationSummary {
    pub is_valid: bool,
    pub issue_count: usize,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractFreezeSummary {
    pub canonical_entries: Vec<String>,
    pub canonical_signature: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractFreezeGateSummary {
    pub is_ready: bool,
    pub blocking_issue_count: usize,
    pub blocking_issues: Vec<String>,
    pub readiness_checks: Vec<String>,
    pub required_validation_refs: Vec<String>,
    pub satisfied_validation_refs: Vec<String>,
    pub pending_validation_refs: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractFreezeEvidenceSummary {
    pub evidence_entries: Vec<String>,
    pub evidence_signature: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractFreezeReportSummary {
    pub status: String,
    pub ready_check_count: usize,
    pub blocking_issue_count: usize,
    pub summary_line: String,
    pub evidence_signature: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractFreezeConsistencySummary {
    pub is_consistent: bool,
    pub issue_count: usize,
    pub issues: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContractFreezeClosureSummary {
    pub is_closed: bool,
    pub final_status: String,
    pub closure_issue_count: usize,
    pub closure_issues: Vec<String>,
}

pub(super) fn runtime_read_model_contract_sections() -> Vec<String> {
    RUNTIME_READ_MODEL_CONTRACT_SECTIONS
        .iter()
        .map(|section| (*section).to_string())
        .collect()
}

pub(super) fn runtime_read_model_section_ordering_rules() -> Vec<RuntimeSectionOrderingRule> {
    RUNTIME_READ_MODEL_SECTION_ORDERING_RULES
        .iter()
        .map(|(target, ordering)| RuntimeSectionOrderingRule {
            target: (*target).to_string(),
            ordering: (*ordering).to_string(),
        })
        .collect()
}

impl RuntimeReadModelInput {
    pub(super) fn refresh_contract_state(&mut self) {
        self.meta.freeze = self.build_freeze_summary();
        self.meta.validation = self.validate_contract();
        self.meta.freeze_gate = self.build_freeze_gate_summary();
        self.meta.freeze_evidence = self.build_freeze_evidence_summary();
        self.meta.freeze_report = self.build_freeze_report_summary();
        self.meta.freeze_consistency = self.build_freeze_consistency_summary();
        self.meta.freeze_closure = self.build_freeze_closure_summary();
    }

    fn validate_contract(&self) -> RuntimeContractValidationSummary {
        let mut issues = Vec::new();

        if self.meta.contract_version != RUNTIME_READ_MODEL_CONTRACT_VERSION {
            issues.push("contract_version 与当前内核常量不一致".to_string());
        }

        let actual_sections = self
            .meta
            .contract_sections
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let expected_sections = RUNTIME_READ_MODEL_CONTRACT_SECTIONS.to_vec();
        if actual_sections != expected_sections {
            issues.push("contract_sections 顺序或内容与冻结结构不一致".to_string());
        }

        if self.meta.ordering_strategy != RUNTIME_READ_MODEL_ORDERING_STRATEGY {
            issues.push("ordering_strategy 不是预期的 deterministic-lexicographic".to_string());
        }

        let expected_rules = runtime_read_model_section_ordering_rules();
        if self.meta.section_ordering_rules.len() != expected_rules.len()
            || self
                .meta
                .section_ordering_rules
                .iter()
                .zip(expected_rules.iter())
                .any(|(actual, expected)| {
                    actual.target != expected.target || actual.ordering != expected.ordering
                })
        {
            issues.push("section_ordering_rules 与冻结规则集不一致".to_string());
        }

        validate_string_set(
            &self.overview.activity.active_execution_group_ids,
            "overview.activity.active_execution_group_ids",
            &mut issues,
        );
        validate_string_set(
            &self.overview.activity.active_task_ids,
            "overview.activity.active_task_ids",
            &mut issues,
        );

        validate_sorted_by_key(
            &self.details.execution_groups,
            |entry| entry.mission_id.clone(),
            "details.execution_groups",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.details.tasks,
            |entry| entry.task_id.clone(),
            "details.tasks",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.details.assignments,
            |entry| entry.assignment_id.clone(),
            "details.assignments",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.details.workers,
            |entry| entry.worker_id.clone(),
            "details.workers",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.details.tools,
            |entry| entry.tool_name.clone(),
            "details.tools",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.details.sessions,
            |entry| entry.session_id.clone(),
            "details.sessions",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.details.workspaces,
            |entry| entry.workspace_id.clone(),
            "details.workspaces",
            &mut issues,
        );

        for entry in &self.details.execution_groups {
            validate_string_set(
                &entry.active_task_ids,
                "details.execution_groups[].active_task_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.context_truncation_parts,
                "details.execution_groups[].context_truncation_parts",
                &mut issues,
            );
            validate_string_set(
                &entry.context_knowledge_ids,
                "details.execution_groups[].context_knowledge_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.context_knowledge_source_paths,
                "details.execution_groups[].context_knowledge_source_paths",
                &mut issues,
            );
            validate_string_set(
                &entry.context_memory_ids,
                "details.execution_groups[].context_memory_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.context_memory_extraction_refs,
                "details.execution_groups[].context_memory_extraction_refs",
                &mut issues,
            );
        }
        for entry in &self.details.assignments {
            validate_string_set(&entry.task_ids, "details.assignments[].task_ids", &mut issues);
        }
        for entry in &self.details.tools {
            validate_string_set(&entry.worker_ids, "details.tools[].worker_ids", &mut issues);
            validate_string_set(&entry.task_ids, "details.tools[].task_ids", &mut issues);
            validate_string_set(&entry.session_ids, "details.tools[].session_ids", &mut issues);
            validate_string_set(
                &entry.workspace_ids,
                "details.tools[].workspace_ids",
                &mut issues,
            );
        }
        for entry in &self.details.sessions {
            validate_string_set(
                &entry.active_execution_group_ids,
                "details.sessions[].active_execution_group_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.active_task_ids,
                "details.sessions[].active_task_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.recovery_ids,
                "details.sessions[].recovery_ids",
                &mut issues,
            );
        }
        for entry in &self.details.workspaces {
            validate_string_set(
                &entry.active_execution_group_ids,
                "details.workspaces[].active_execution_group_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.active_task_ids,
                "details.workspaces[].active_task_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.recovery_ids,
                "details.workspaces[].recovery_ids",
                &mut issues,
            );
            validate_string_set(
                &entry.execution_chain_refs,
                "details.workspaces[].execution_chain_refs",
                &mut issues,
            );
        }
        for entry in &self.details.workers {
            validate_string_set(
                &entry.executor_supported_step_kinds,
                "details.workers[].executor_supported_step_kinds",
                &mut issues,
            );
            validate_string_set(
                &entry.executor_requested_step_kinds,
                "details.workers[].executor_requested_step_kinds",
                &mut issues,
            );
        }

        validate_string_set(
            &self.operations.dispatch.active_assignment_ids,
            "operations.dispatch.active_assignment_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.failed_execution_group_ids,
            "operations.attention.failed_execution_group_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.failed_task_ids,
            "operations.attention.failed_task_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.failed_assignment_ids,
            "operations.attention.failed_assignment_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.failed_worker_ids,
            "operations.attention.failed_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.blocked_tool_names,
            "operations.attention.blocked_tool_names",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.governance_blocked_task_ids,
            "operations.attention.governance_blocked_task_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.governance_approval_required_task_ids,
            "operations.attention.governance_approval_required_task_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.governance_rejected_task_ids,
            "operations.attention.governance_rejected_task_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.governance_blocked_worker_ids,
            "operations.attention.governance_blocked_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.governance_approval_required_worker_ids,
            "operations.attention.governance_approval_required_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.governance_rejected_worker_ids,
            "operations.attention.governance_rejected_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.rejected_skill_dispatch_worker_ids,
            "operations.attention.rejected_skill_dispatch_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.failed_skill_dispatch_worker_ids,
            "operations.attention.failed_skill_dispatch_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.degraded_executor_worker_ids,
            "operations.attention.degraded_executor_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.unavailable_executor_worker_ids,
            "operations.attention.unavailable_executor_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.attention.pending_recovery_ids,
            "operations.attention.pending_recovery_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.work_queues.running_execution_group_ids,
            "operations.work_queues.running_execution_group_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.work_queues.running_task_ids,
            "operations.work_queues.running_task_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.work_queues.running_assignment_ids,
            "operations.work_queues.running_assignment_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.work_queues.active_worker_ids,
            "operations.work_queues.active_worker_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.work_queues.pending_recovery_ids,
            "operations.work_queues.pending_recovery_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.resume_observation.affected_execution_group_ids,
            "operations.resume_observation.affected_execution_group_ids",
            &mut issues,
        );
        validate_string_set(
            &self.operations.resume_observation.affected_worker_ids,
            "operations.resume_observation.affected_worker_ids",
            &mut issues,
        );

        validate_sorted_by_key(
            &self.recovery.entries,
            |entry| entry.sequence,
            "recovery.entries",
            &mut issues,
        );
        validate_sorted_by_key(
            &self.recovery.summaries,
            |entry| entry.recovery_id.clone(),
            "recovery.summaries",
            &mut issues,
        );
        validate_string_set(
            &self.recovery.active_recovery_ids,
            "recovery.active_recovery_ids",
            &mut issues,
        );

        if self.meta.ledger.schema_version != RUNTIME_LEDGER_SCHEMA_VERSION {
            issues.push("ledger.schema_version 与当前账本常量不一致".to_string());
        }

        RuntimeContractValidationSummary {
            is_valid: issues.is_empty(),
            issue_count: issues.len(),
            issues,
        }
    }

    fn build_freeze_summary(&self) -> RuntimeContractFreezeSummary {
        let mut canonical_entries = Vec::new();
        canonical_entries.push(format!(
            "contract_version={}",
            self.meta.contract_version
        ));
        canonical_entries.push(format!(
            "contract_sections={}",
            self.meta.contract_sections.join(",")
        ));
        canonical_entries.push(format!(
            "ordering_strategy={}",
            self.meta.ordering_strategy
        ));
        for rule in &self.meta.section_ordering_rules {
            canonical_entries.push(format!("rule:{}={}", rule.target, rule.ordering));
        }

        let canonical_signature = canonical_entries.join(" | ");
        RuntimeContractFreezeSummary {
            canonical_entries,
            canonical_signature,
        }
    }

    fn build_freeze_gate_summary(&self) -> RuntimeContractFreezeGateSummary {
        let mut blocking_issues = Vec::new();
        let mut readiness_checks = Vec::new();
        let required_validation_refs = RUNTIME_READ_MODEL_REQUIRED_VALIDATION_REFS
            .iter()
            .map(|reference| (*reference).to_string())
            .collect::<Vec<_>>();
        let mut satisfied_validation_refs = Vec::new();

        if self.meta.contract_version == RUNTIME_READ_MODEL_CONTRACT_VERSION {
            readiness_checks.push("contract_version 已冻结".to_string());
            satisfied_validation_refs.push("runtime_read_model.contract_version".to_string());
        } else {
            blocking_issues.push("contract_version 未命中当前冻结版本".to_string());
        }

        let actual_sections = self
            .meta
            .contract_sections
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let expected_sections = RUNTIME_READ_MODEL_CONTRACT_SECTIONS.to_vec();
        if actual_sections == expected_sections {
            readiness_checks.push("contract_sections 已冻结".to_string());
            satisfied_validation_refs.push("runtime_read_model.contract_sections".to_string());
        } else {
            blocking_issues.push("contract_sections 未命中当前冻结结构".to_string());
        }

        if self.meta.ordering_strategy == RUNTIME_READ_MODEL_ORDERING_STRATEGY {
            readiness_checks.push("ordering_strategy 已冻结".to_string());
            satisfied_validation_refs.push("runtime_read_model.ordering_strategy".to_string());
        } else {
            blocking_issues.push("ordering_strategy 未命中当前冻结策略".to_string());
        }

        if !self.meta.section_ordering_rules.is_empty() {
            readiness_checks.push("section_ordering_rules 已声明".to_string());
            satisfied_validation_refs.push("runtime_read_model.section_ordering_rules".to_string());
        } else {
            blocking_issues.push("section_ordering_rules 为空".to_string());
        }

        if !self.meta.freeze.canonical_signature.is_empty() {
            readiness_checks.push("freeze.canonical_signature 已生成".to_string());
            satisfied_validation_refs.push("runtime_read_model.freeze_signature".to_string());
        } else {
            blocking_issues.push("freeze.canonical_signature 为空".to_string());
        }

        if self.meta.validation.is_valid {
            readiness_checks.push("validation 已通过".to_string());
            satisfied_validation_refs.push("runtime_read_model.validation_passed".to_string());
        } else {
            blocking_issues.extend(
                self.meta
                    .validation
                    .issues
                    .iter()
                    .map(|issue| format!("validation 未通过: {issue}")),
            );
        }

        let pending_validation_refs = required_validation_refs
            .iter()
            .filter(|reference| {
                !satisfied_validation_refs
                    .iter()
                    .any(|satisfied| satisfied == *reference)
            })
            .cloned()
            .collect::<Vec<_>>();

        RuntimeContractFreezeGateSummary {
            is_ready: blocking_issues.is_empty(),
            blocking_issue_count: blocking_issues.len(),
            blocking_issues,
            readiness_checks,
            required_validation_refs,
            satisfied_validation_refs,
            pending_validation_refs,
        }
    }

    fn build_freeze_evidence_summary(&self) -> RuntimeContractFreezeEvidenceSummary {
        let mut evidence_entries = Vec::new();
        evidence_entries.push(format!(
            "contract_version={}",
            self.meta.contract_version
        ));
        evidence_entries.push(format!(
            "freeze_signature={}",
            self.meta.freeze.canonical_signature
        ));
        evidence_entries.push(format!(
            "validation_passed={}",
            self.meta.validation.is_valid
        ));
        evidence_entries.push(format!(
            "freeze_ready={}",
            self.meta.freeze_gate.is_ready
        ));
        evidence_entries.push(format!(
            "satisfied_validation_refs={}",
            self.meta.freeze_gate.satisfied_validation_refs.join(",")
        ));
        evidence_entries.push(format!(
            "pending_validation_refs={}",
            self.meta.freeze_gate.pending_validation_refs.join(",")
        ));

        let evidence_signature = evidence_entries.join(" | ");
        RuntimeContractFreezeEvidenceSummary {
            evidence_entries,
            evidence_signature,
        }
    }

    fn build_freeze_report_summary(&self) -> RuntimeContractFreezeReportSummary {
        let status = if self.meta.freeze_gate.is_ready {
            "ready"
        } else {
            "blocked"
        }
        .to_string();
        let summary_line = format!(
            "status={status}; ready_checks={}; blocking_issues={}",
            self.meta.freeze_gate.readiness_checks.len(),
            self.meta.freeze_gate.blocking_issue_count
        );

        RuntimeContractFreezeReportSummary {
            status,
            ready_check_count: self.meta.freeze_gate.readiness_checks.len(),
            blocking_issue_count: self.meta.freeze_gate.blocking_issue_count,
            summary_line,
            evidence_signature: self.meta.freeze_evidence.evidence_signature.clone(),
        }
    }

    fn build_freeze_consistency_summary(&self) -> RuntimeContractFreezeConsistencySummary {
        let mut issues = Vec::new();

        if self.meta.freeze_gate.is_ready != self.meta.validation.is_valid {
            issues.push("freeze_gate.is_ready 与 validation.is_valid 不一致".to_string());
        }

        let expected_status = if self.meta.freeze_gate.is_ready {
            "ready"
        } else {
            "blocked"
        };
        if self.meta.freeze_report.status != expected_status {
            issues.push("freeze_report.status 与 freeze_gate 结论不一致".to_string());
        }

        if self.meta.freeze_report.evidence_signature != self.meta.freeze_evidence.evidence_signature
        {
            issues.push("freeze_report.evidence_signature 与 freeze_evidence 不一致".to_string());
        }

        if self.meta.freeze_gate.blocking_issue_count != self.meta.freeze_gate.blocking_issues.len()
        {
            issues.push("freeze_gate.blocking_issue_count 与 blocking_issues 数量不一致".to_string());
        }

        if self.meta.freeze_report.ready_check_count != self.meta.freeze_gate.readiness_checks.len()
        {
            issues.push("freeze_report.ready_check_count 与 freeze_gate.readiness_checks 数量不一致".to_string());
        }

        if self.meta.freeze.canonical_signature.is_empty() {
            issues.push("freeze.canonical_signature 为空".to_string());
        }

        RuntimeContractFreezeConsistencySummary {
            is_consistent: issues.is_empty(),
            issue_count: issues.len(),
            issues,
        }
    }

    fn build_freeze_closure_summary(&self) -> RuntimeContractFreezeClosureSummary {
        let mut closure_issues = Vec::new();

        if !self.meta.validation.is_valid {
            closure_issues.push("validation 未通过，冻结链路不能闭环".to_string());
        }
        if !self.meta.freeze_gate.is_ready {
            closure_issues.push("freeze_gate 未就绪，冻结链路不能闭环".to_string());
        }
        if !self.meta.freeze_consistency.is_consistent {
            closure_issues.push("freeze_consistency 未通过，冻结链路不能闭环".to_string());
        }
        if self.meta.freeze_report.status != "ready" {
            closure_issues.push("freeze_report 不是 ready，冻结链路不能闭环".to_string());
        }
        if self.meta.freeze_evidence.evidence_signature.is_empty() {
            closure_issues.push("freeze_evidence 缺少 evidence_signature".to_string());
        }

        let is_closed = closure_issues.is_empty();
        let final_status = if is_closed { "closed" } else { "open" }.to_string();

        RuntimeContractFreezeClosureSummary {
            is_closed,
            final_status,
            closure_issue_count: closure_issues.len(),
            closure_issues,
        }
    }
}

fn validate_string_set(values: &[String], label: &str, issues: &mut Vec<String>) {
    if !values.windows(2).all(|window| window[0] < window[1]) {
        issues.push(format!("{label} 未满足升序唯一集合约束"));
    }
}

fn validate_sorted_by_key<T, K, F>(values: &[T], mut key_fn: F, label: &str, issues: &mut Vec<String>)
where
    K: PartialOrd,
    F: FnMut(&T) -> K,
{
    if !values
        .windows(2)
        .all(|window| key_fn(&window[0]) <= key_fn(&window[1]))
    {
        issues.push(format!("{label} 未满足声明的排序规则"));
    }
}
