use super::types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

const CURRENT_SCHEMA_VERSION: u32 = 2;

static PLAN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

struct NormalizedAttemptSelector {
    scope: PlanAttemptScope,
    target_id: String,
    assignment_id: Option<String>,
    task_id: Option<String>,
    reason: Option<String>,
}

pub struct PlanLedgerService {
    plans: HashMap<String, HashMap<String, PlanRecord>>,
    index: HashMap<String, Vec<PlanIndexEntry>>,
}

#[derive(Clone, Debug)]
struct PlanIndexEntry {
    plan_id: String,
    turn_id: String,
    mission_id: Option<String>,
    status: PlanStatus,
    version: u32,
    updated_at: u64,
}

impl PlanLedgerService {
    pub fn new() -> Self {
        Self {
            plans: HashMap::new(),
            index: HashMap::new(),
        }
    }

    pub fn create_draft(&mut self, input: CreatePlanDraftInput) -> PlanRecord {
        let now = now_millis();
        let plan_id = generate_plan_id(now);

        let session_index = self.index.entry(input.session_id.clone()).or_default();
        let latest_for_turn: Option<&PlanIndexEntry> = session_index
            .iter()
            .filter(|e| e.turn_id == input.turn_id)
            .max_by_key(|e| e.version);

        let version = latest_for_turn.map_or(1, |e| e.version + 1);
        let parent_plan_id = latest_for_turn.map(|e| e.plan_id.clone());

        if let Some(prev) = latest_for_turn {
            let prev_id = prev.plan_id.clone();
            self.mark_superseded(&input.session_id, &prev_id);
        }

        let summary = input
            .summary
            .as_deref()
            .unwrap_or(&input.prompt)
            .trim()
            .to_string();
        let summary = if summary.is_empty() {
            "未命名计划".to_string()
        } else {
            summary
        };

        let acceptance_criteria = normalize_acceptance_criteria(input.acceptance_criteria);

        let record = PlanRecord {
            plan_id: plan_id.clone(),
            session_id: input.session_id.clone(),
            mission_id: input.mission_id,
            turn_id: input.turn_id.clone(),
            schema_version: CURRENT_SCHEMA_VERSION,
            revision: 1,
            version,
            parent_plan_id,
            mode: input.mode,
            status: PlanStatus::Draft,
            prompt_digest: build_prompt_digest(&input.prompt),
            summary,
            analysis: input.analysis.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            constraints: normalize_string_vec(input.constraints),
            risk_level: input.risk_level,
            review: None,
            runtime: create_initial_runtime_state(acceptance_criteria, now),
            formatted_plan: input.formatted_plan.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            items: Vec::new(),
            attempts: Vec::new(),
            links: PlanLinks::default(),
            recovery_protected: false,
            created_at: now,
            updated_at: now,
        };

        self.store_plan(record.clone());
        record
    }

    pub fn mark_awaiting_confirmation(
        &mut self,
        session_id: &str,
        plan_id: &str,
        formatted_plan: Option<&str>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }
        if !try_transition_plan_status(record, PlanStatus::AwaitingConfirmation) {
            return None;
        }
        if let Some(fp) = formatted_plan {
            let trimmed = fp.trim();
            if !trimmed.is_empty() {
                record.formatted_plan = Some(trimmed.to_string());
            }
        }
        record.updated_at = now_millis();
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn approve(
        &mut self,
        session_id: &str,
        plan_id: &str,
        reviewer: Option<&str>,
        reason: Option<&str>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }
        if !try_transition_plan_status(record, PlanStatus::Approved) {
            return None;
        }
        let now = now_millis();
        record.review = Some(PlanReview {
            status: PlanReviewStatus::Approved,
            reviewer: Some(reviewer.unwrap_or("system:auto").to_string()),
            reason: reason.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            reviewed_at: now,
        });
        record.updated_at = now;
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn reject(
        &mut self,
        session_id: &str,
        plan_id: &str,
        reviewer: Option<&str>,
        reason: Option<&str>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }
        if !try_transition_plan_status(record, PlanStatus::Rejected) {
            return None;
        }
        let now = now_millis();
        record.review = Some(PlanReview {
            status: PlanReviewStatus::Rejected,
            reviewer: Some(reviewer.unwrap_or("user").to_string()),
            reason: Some(
                reason
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "用户拒绝执行计划".to_string()),
            ),
            reviewed_at: now,
        });
        record.updated_at = now;
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn mark_executing(&mut self, session_id: &str, plan_id: &str) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;

        let is_recovery_resume = record.recovery_protected && record.status.is_terminal();
        if !is_recovery_resume && record.status.is_terminal() {
            return None;
        }

        if is_recovery_resume {
            record.status = PlanStatus::Executing;
        } else if !try_transition_plan_status(record, PlanStatus::Executing) {
            return None;
        }

        if record.recovery_protected {
            record.recovery_protected = false;
        }

        record.updated_at = now_millis();
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn set_recovery_protection(
        &mut self,
        session_id: &str,
        plan_id: &str,
        protect: bool,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.recovery_protected == protect {
            return Some(record.clone());
        }
        record.recovery_protected = protect;
        record.updated_at = now_millis();
        record.revision += 1;
        Some(record.clone())
    }

    pub fn bind_mission(
        &mut self,
        session_id: &str,
        plan_id: &str,
        mission_id: &str,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        record.mission_id = Some(mission_id.to_string());
        record.updated_at = now_millis();
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn update_summary(
        &mut self,
        session_id: &str,
        plan_id: &str,
        summary: &str,
    ) -> Option<PlanRecord> {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return None;
        }
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }
        record.summary = trimmed.to_string();
        record.updated_at = now_millis();
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn upsert_dispatch_item(
        &mut self,
        session_id: &str,
        plan_id: &str,
        input: DispatchPlanItemInput,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }

        let now = now_millis();
        let item_id = input.item_id.trim().to_string();
        if item_id.is_empty() {
            return Some(record.clone());
        }

        if let Some(existing) = record.items.iter_mut().find(|i| i.item_id == item_id) {
            let title = input.title.trim();
            if !title.is_empty() {
                existing.title = title.to_string();
            }
            existing.owner = input.worker;
            if input.category.is_some() {
                existing.category = input.category;
            }
            if let Some(deps) = input.depends_on {
                existing.depends_on = deps;
            }
            if let Some(hints) = input.scope_hints {
                existing.scope_hints = Some(hints);
            }
            if let Some(files) = input.target_files {
                existing.target_files = Some(files);
            }
            if let Some(rm) = input.requires_modification {
                existing.requires_modification = rm;
            }
            existing.updated_at = now;
        } else {
            let item = PlanItem {
                item_id: item_id.clone(),
                title: {
                    let t = input.title.trim().to_string();
                    if t.is_empty() { item_id.clone() } else { t }
                },
                owner: input.worker,
                category: input.category,
                depends_on: input.depends_on.unwrap_or_default(),
                scope_hints: input.scope_hints,
                target_files: input.target_files,
                requires_modification: input.requires_modification.unwrap_or(false),
                status: PlanItemStatus::Pending,
                progress: 0.0,
                assignment_id: Some(item_id.clone()),
                task_ids: Vec::new(),
                task_statuses: HashMap::new(),
                created_at: now,
                updated_at: now,
            };
            record.items.push(item);
        }

        add_unique(&mut record.links.assignment_ids, &item_id);

        if record.status == PlanStatus::Draft {
            try_transition_plan_status(record, PlanStatus::Approved);
        }

        record.updated_at = now;
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn update_assignment_status(
        &mut self,
        session_id: &str,
        plan_id: &str,
        assignment_id: &str,
        status: PlanItemStatus,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }

        let normalized = assignment_id.trim();
        if normalized.is_empty() {
            return Some(record.clone());
        }

        let item = record
            .items
            .iter_mut()
            .find(|i| i.assignment_id.as_deref() == Some(normalized) || i.item_id == normalized)?;

        item.status = status;
        match status {
            PlanItemStatus::Completed => item.progress = 100.0,
            PlanItemStatus::Failed | PlanItemStatus::Cancelled => {
                if item.progress < 1.0 {
                    item.progress = 1.0;
                }
            }
            _ => {}
        }
        item.updated_at = now_millis();
        record.updated_at = item.updated_at;

        let next = compute_plan_status(record, Some(record.status));
        try_transition_plan_status(record, next);

        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn update_task_status(
        &mut self,
        session_id: &str,
        plan_id: &str,
        assignment_id: &str,
        task_id: &str,
        status: &str,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }

        let normalized_task = task_id.trim().to_string();
        let normalized_assignment = assignment_id.trim().to_string();
        if normalized_task.is_empty() || normalized_assignment.is_empty() {
            return Some(record.clone());
        }

        let item = record.items.iter_mut().find(|i| {
            i.assignment_id.as_deref() == Some(&normalized_assignment)
                || i.item_id == normalized_assignment
        })?;

        add_unique(&mut item.task_ids, &normalized_task);
        add_unique(&mut record.links.task_ids, &normalized_task);
        item.task_statuses
            .insert(normalized_task, status.to_string());
        item.progress = compute_item_progress(item);
        item.status = compute_item_status(item);
        item.updated_at = now_millis();
        record.updated_at = item.updated_at;

        if matches!(
            record.status,
            PlanStatus::Approved | PlanStatus::AwaitingConfirmation
        ) {
            try_transition_plan_status(record, PlanStatus::Executing);
        }

        let next = compute_plan_status(record, Some(record.status));
        try_transition_plan_status(record, next);

        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn start_attempt(
        &mut self,
        session_id: &str,
        plan_id: &str,
        input: PlanAttemptStartInput,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return None;
        }

        let normalized = normalize_attempt_input(&input);
        let now = now_millis();

        let existing = find_latest_attempt_mut(
            &mut record.attempts,
            &normalized,
            Some(&[PlanAttemptStatus::Created, PlanAttemptStatus::Inflight]),
        );

        if let Some(attempt) = existing {
            if attempt.status == PlanAttemptStatus::Created {
                apply_attempt_transition(attempt, PlanAttemptStatus::Inflight, normalized.reason.as_deref());
            }
        } else {
            let sequence = next_attempt_sequence(&record.attempts, &normalized);
            let attempt_id = generate_attempt_id(&normalized.scope, &normalized.target_id, sequence);
            let mut attempt = PlanAttemptRecord {
                attempt_id,
                scope: normalized.scope,
                target_id: normalized.target_id.clone(),
                assignment_id: normalized.assignment_id.clone(),
                task_id: normalized.task_id.clone(),
                sequence,
                status: PlanAttemptStatus::Created,
                reason: normalized.reason.clone(),
                error: None,
                evidence_ids: Vec::new(),
                metadata: None,
                created_at: now,
                started_at: None,
                ended_at: None,
                updated_at: now,
            };
            apply_attempt_transition(&mut attempt, PlanAttemptStatus::Inflight, normalized.reason.as_deref());
            record.attempts.push(attempt);
        }

        record.updated_at = now;
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn complete_latest_attempt(
        &mut self,
        session_id: &str,
        plan_id: &str,
        input: PlanAttemptCompleteInput,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;

        let normalized = normalize_attempt_input_from_complete(&input);
        let now = now_millis();

        let has_active = record.attempts.iter().any(|a| {
            a.scope == normalized.scope
                && a.target_id == normalized.target_id
                && matches!(
                    a.status,
                    PlanAttemptStatus::Created | PlanAttemptStatus::Inflight
                )
        });

        if !has_active {
            let has_terminal = record.attempts.iter().any(|a| {
                a.scope == normalized.scope
                    && a.target_id == normalized.target_id
                    && a.status.is_terminal()
            });

            if has_terminal {
                return Some(record.clone());
            }

            let sequence = next_attempt_sequence(&record.attempts, &normalized);
            let attempt_id =
                generate_attempt_id(&normalized.scope, &normalized.target_id, sequence);
            let mut attempt = PlanAttemptRecord {
                attempt_id,
                scope: normalized.scope,
                target_id: normalized.target_id.clone(),
                assignment_id: normalized.assignment_id.clone(),
                task_id: normalized.task_id.clone(),
                sequence,
                status: PlanAttemptStatus::Created,
                reason: normalized.reason.clone(),
                error: None,
                evidence_ids: Vec::new(),
                metadata: None,
                created_at: now,
                started_at: None,
                ended_at: None,
                updated_at: now,
            };
            apply_attempt_transition(
                &mut attempt,
                PlanAttemptStatus::Inflight,
                normalized.reason.as_deref(),
            );
            apply_attempt_transition(&mut attempt, input.status, input.error.as_deref());
            if let Some(ev) = &input.evidence_ids {
                attempt.evidence_ids = ev.clone();
            }
            record.attempts.push(attempt);
        } else {
            for attempt in &mut record.attempts {
                if attempt.scope != normalized.scope || attempt.target_id != normalized.target_id {
                    continue;
                }
                if !matches!(
                    attempt.status,
                    PlanAttemptStatus::Created | PlanAttemptStatus::Inflight
                ) {
                    continue;
                }
                if attempt.status == PlanAttemptStatus::Created {
                    apply_attempt_transition(
                        attempt,
                        PlanAttemptStatus::Inflight,
                        normalized.reason.as_deref(),
                    );
                }
                apply_attempt_transition(attempt, input.status, input.error.as_deref());
                if let Some(ev) = &input.evidence_ids {
                    for e in ev {
                        if !attempt.evidence_ids.contains(e) {
                            attempt.evidence_ids.push(e.clone());
                        }
                    }
                }
                break;
            }
        }

        record.updated_at = now;
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn finalize(
        &mut self,
        session_id: &str,
        plan_id: &str,
        status: PlanStatus,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        if record.status.is_terminal() {
            return Some(record.clone());
        }

        let now = now_millis();

        record.runtime.termination = PlanRuntimeTerminationState {
            reason: Some(format!("plan-finalized:{status:?}")),
            updated_at: Some(now),
            ..Default::default()
        };

        if status == PlanStatus::Completed {
            record.runtime.acceptance.summary = PlanAcceptanceSummary::Passed;
            record.runtime.acceptance.updated_at = now;
        } else if status == PlanStatus::Failed {
            record.runtime.acceptance.summary = PlanAcceptanceSummary::Failed;
            record.runtime.acceptance.updated_at = now;
        }

        finalize_inflight_attempts(record, status);

        try_transition_plan_status(record, status);
        record.updated_at = now;
        record.revision += 1;
        let result = record.clone();
        self.update_index(&result);
        Some(result)
    }

    pub fn update_runtime_review(
        &mut self,
        session_id: &str,
        plan_id: &str,
        state: ReviewState,
        round: Option<u32>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        let now = now_millis();
        record.runtime.review.state = state;
        if let Some(r) = round {
            record.runtime.review.round = r;
        }
        if matches!(state, ReviewState::Running | ReviewState::Accepted | ReviewState::Rejected) {
            record.runtime.review.last_reviewed_at = Some(now);
        }
        record.updated_at = now;
        record.revision += 1;
        Some(record.clone())
    }

    pub fn update_runtime_replan(
        &mut self,
        session_id: &str,
        plan_id: &str,
        state: ReplanState,
        reason: Option<&str>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        let now = now_millis();
        record.runtime.replan.state = state;
        record.runtime.replan.reason = if state == ReplanState::None && reason.is_none() {
            None
        } else {
            reason.map(|s| s.to_string())
        };
        record.runtime.replan.updated_at = Some(now);
        record.updated_at = now;
        record.revision += 1;
        Some(record.clone())
    }

    pub fn update_runtime_wait(
        &mut self,
        session_id: &str,
        plan_id: &str,
        state: WaitState,
        reason_code: Option<&str>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        let now = now_millis();
        record.runtime.wait.state = state;
        record.runtime.wait.reason_code = if state == WaitState::None && reason_code.is_none() {
            None
        } else {
            reason_code.map(|s| s.to_string())
        };
        record.runtime.wait.updated_at = Some(now);
        record.updated_at = now;
        record.revision += 1;
        Some(record.clone())
    }

    pub fn update_runtime_phase(
        &mut self,
        session_id: &str,
        plan_id: &str,
        phase: PlanRuntimePhaseState,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        let now = now_millis();
        let mut phase = phase;
        phase.updated_at = Some(now);
        if phase.continuation_intent == ContinuationIntent::Stop {
            phase.next_index = None;
            phase.next_title = None;
            phase.remaining_phases = Vec::new();
        }
        record.runtime.phase = phase;
        record.updated_at = now;
        record.revision += 1;
        Some(record.clone())
    }

    pub fn update_runtime_acceptance(
        &mut self,
        session_id: &str,
        plan_id: &str,
        criteria: Option<Vec<AcceptanceCriterion>>,
        summary: Option<PlanAcceptanceSummary>,
    ) -> Option<PlanRecord> {
        let record = self.load_plan_mut(session_id, plan_id)?;
        let now = now_millis();
        if let Some(c) = criteria {
            record.runtime.acceptance.criteria = c;
            if summary.is_none() {
                record.runtime.acceptance.summary =
                    compute_acceptance_summary(&record.runtime.acceptance.criteria);
            }
        }
        if let Some(s) = summary {
            record.runtime.acceptance.summary = s;
        }
        record.runtime.acceptance.updated_at = now;
        record.updated_at = now;
        record.revision += 1;
        Some(record.clone())
    }

    pub fn get_plan(&self, session_id: &str, plan_id: &str) -> Option<&PlanRecord> {
        self.plans.get(session_id)?.get(plan_id)
    }

    pub fn list_plans(&self, session_id: &str, limit: usize) -> Vec<&PlanRecord> {
        let limit = limit.max(1);
        let Some(index) = self.index.get(session_id) else {
            return Vec::new();
        };
        let mut sorted: Vec<&PlanIndexEntry> = index.iter().collect();
        sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sorted.truncate(limit);

        let session_plans = match self.plans.get(session_id) {
            Some(m) => m,
            None => return Vec::new(),
        };
        sorted
            .iter()
            .filter_map(|e| session_plans.get(&e.plan_id))
            .collect()
    }

    pub fn get_active_plan(&self, session_id: &str) -> Option<&PlanRecord> {
        let index = self.index.get(session_id)?;
        let mut sorted: Vec<&PlanIndexEntry> = index.iter().collect();
        sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let session_plans = self.plans.get(session_id)?;
        for entry in sorted {
            if entry.status.is_terminal() {
                continue;
            }
            if let Some(plan) = session_plans.get(&entry.plan_id) {
                return Some(plan);
            }
        }
        None
    }

    pub fn get_latest_plan(&self, session_id: &str) -> Option<&PlanRecord> {
        let index = self.index.get(session_id)?;
        let latest = index.iter().max_by_key(|e| e.updated_at)?;
        self.plans.get(session_id)?.get(&latest.plan_id)
    }

    pub fn get_latest_plan_by_mission(
        &self,
        session_id: &str,
        mission_id: &str,
        include_terminal: bool,
    ) -> Option<&PlanRecord> {
        let index = self.index.get(session_id)?;
        let session_plans = self.plans.get(session_id)?;
        let mut sorted: Vec<&PlanIndexEntry> = index
            .iter()
            .filter(|e| e.mission_id.as_deref() == Some(mission_id))
            .collect();
        sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        for entry in sorted {
            if !include_terminal && entry.status.is_terminal() {
                continue;
            }
            if let Some(plan) = session_plans.get(&entry.plan_id) {
                if !include_terminal && plan.status.is_terminal() {
                    continue;
                }
                return Some(plan);
            }
        }
        None
    }

    pub fn format_plan_for_display(plan: &PlanRecord) -> String {
        let mut lines = Vec::new();
        lines.push("## 计划摘要".to_string());
        lines.push(if plan.summary.is_empty() {
            "未命名计划".to_string()
        } else {
            plan.summary.clone()
        });

        if let Some(analysis) = &plan.analysis {
            lines.push(String::new());
            lines.push("### 分析".to_string());
            lines.push(analysis.clone());
        }

        if !plan.constraints.is_empty() {
            lines.push(String::new());
            lines.push("### 约束".to_string());
            for c in &plan.constraints {
                lines.push(format!("- {c}"));
            }
        }

        let descriptions: Vec<&str> = plan
            .runtime
            .acceptance
            .criteria
            .iter()
            .map(|c| c.description.as_str())
            .collect();
        if !descriptions.is_empty() {
            lines.push(String::new());
            lines.push("### 验收".to_string());
            for d in descriptions {
                lines.push(format!("- {d}"));
            }
        }

        if !plan.items.is_empty() {
            lines.push(String::new());
            lines.push("### 任务分解".to_string());
            for item in &plan.items {
                lines.push(format!("1. [{}] {}", item.owner, item.title));
            }
        }

        lines.join("\n")
    }

    pub fn reconcile_by_missions(
        &mut self,
        session_id: &str,
        missions: &[(&str, &str)],
    ) -> usize {
        let mut terminal_map: HashMap<&str, &str> = HashMap::new();
        for (id, status) in missions {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            match *status {
                "completed" | "failed" | "cancelled" => {
                    terminal_map.insert(id, status);
                }
                _ => {}
            }
        }
        if terminal_map.is_empty() {
            return 0;
        }

        let Some(index) = self.index.get(session_id) else {
            return 0;
        };

        let plan_ids: Vec<(String, String)> = index
            .iter()
            .filter(|e| !e.status.is_terminal())
            .filter_map(|e| {
                let mid = e.mission_id.as_deref()?;
                let status = terminal_map.get(mid)?;
                Some((e.plan_id.clone(), status.to_string()))
            })
            .collect();

        let mut reconciled = 0;
        for (pid, mission_status) in plan_ids {
            let Some(record) = self.load_plan_mut(session_id, &pid) else {
                continue;
            };
            if record.status.is_terminal() {
                continue;
            }

            let next = match mission_status.as_str() {
                "cancelled" => PlanStatus::Cancelled,
                "completed" => {
                    let has_failed = record.items.iter().any(|i| {
                        matches!(i.status, PlanItemStatus::Failed | PlanItemStatus::Cancelled)
                    });
                    if has_failed {
                        PlanStatus::PartiallyCompleted
                    } else {
                        PlanStatus::Completed
                    }
                }
                _ => {
                    let has_completed = record.items.iter().any(|i| {
                        matches!(
                            i.status,
                            PlanItemStatus::Completed | PlanItemStatus::Skipped
                        )
                    });
                    if has_completed {
                        PlanStatus::PartiallyCompleted
                    } else {
                        PlanStatus::Failed
                    }
                }
            };

            finalize_inflight_attempts(record, next);
            if try_transition_plan_status(record, next) {
                record.updated_at = now_millis();
                record.revision += 1;
                let result = record.clone();
                self.update_index(&result);
                reconciled += 1;
            }
        }
        reconciled
    }

    pub fn plan_count(&self, session_id: &str) -> usize {
        self.plans.get(session_id).map_or(0, |m| m.len())
    }

    fn mark_superseded(&mut self, session_id: &str, plan_id: &str) {
        if let Some(record) = self.load_plan_mut(session_id, plan_id) {
            if !record.status.is_terminal() {
                record.status = PlanStatus::Superseded;
                record.updated_at = now_millis();
                record.revision += 1;
                let result = record.clone();
                self.update_index(&result);
            }
        }
    }

    fn store_plan(&mut self, record: PlanRecord) {
        let entry = PlanIndexEntry {
            plan_id: record.plan_id.clone(),
            turn_id: record.turn_id.clone(),
            mission_id: record.mission_id.clone(),
            status: record.status,
            version: record.version,
            updated_at: record.updated_at,
        };
        let session_index = self.index.entry(record.session_id.clone()).or_default();
        session_index.push(entry);
        self.plans
            .entry(record.session_id.clone())
            .or_default()
            .insert(record.plan_id.clone(), record);
    }

    fn load_plan_mut(&mut self, session_id: &str, plan_id: &str) -> Option<&mut PlanRecord> {
        self.plans.get_mut(session_id)?.get_mut(plan_id)
    }

    fn update_index(&mut self, record: &PlanRecord) {
        let session_index = self.index.entry(record.session_id.clone()).or_default();
        if let Some(entry) = session_index.iter_mut().find(|e| e.plan_id == record.plan_id) {
            entry.status = record.status;
            entry.mission_id.clone_from(&record.mission_id);
            entry.updated_at = record.updated_at;
        }
    }
}

impl Default for PlanLedgerService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl PlanLedgerService {
    pub fn force_status_for_test(&mut self, session_id: &str, plan_id: &str, status: PlanStatus) {
        if let Some(record) = self.load_plan_mut(session_id, plan_id) {
            record.status = status;
        }
    }
}

fn try_transition_plan_status(record: &mut PlanRecord, next: PlanStatus) -> bool {
    if record.status == next {
        return true;
    }
    if record.recovery_protected && next.is_terminal() {
        return false;
    }
    let allowed = record.status.allowed_transitions();
    if !allowed.contains(&next) {
        return false;
    }
    record.status = next;
    true
}

fn apply_attempt_transition(
    attempt: &mut PlanAttemptRecord,
    next: PlanAttemptStatus,
    reason: Option<&str>,
) -> bool {
    if attempt.status == next {
        if let Some(r) = reason {
            if attempt.reason.as_deref() != Some(r) {
                attempt.reason = Some(r.to_string());
                attempt.updated_at = now_millis();
                return true;
            }
        }
        return false;
    }
    let allowed = attempt.status.allowed_transitions();
    if !allowed.contains(&next) {
        return false;
    }
    let now = now_millis();
    attempt.status = next;
    if next == PlanAttemptStatus::Inflight && attempt.started_at.is_none() {
        attempt.started_at = Some(now);
    }
    if next.is_terminal() {
        attempt.ended_at = Some(now);
    }
    if let Some(r) = reason {
        attempt.reason = Some(r.to_string());
    }
    attempt.updated_at = now;
    true
}

fn finalize_inflight_attempts(record: &mut PlanRecord, plan_status: PlanStatus) {
    let terminal = match plan_status {
        PlanStatus::Completed => PlanAttemptStatus::Cancelled,
        PlanStatus::Cancelled => PlanAttemptStatus::Cancelled,
        _ => PlanAttemptStatus::Failed,
    };
    let reason = format!("plan-finalized:{plan_status:?}");
    for attempt in &mut record.attempts {
        if matches!(
            attempt.status,
            PlanAttemptStatus::Created | PlanAttemptStatus::Inflight
        ) {
            if attempt.status == PlanAttemptStatus::Created {
                apply_attempt_transition(attempt, PlanAttemptStatus::Inflight, Some(&reason));
            }
            apply_attempt_transition(attempt, terminal, Some(&reason));
        }
    }
}

fn compute_plan_status(record: &PlanRecord, fallback: Option<PlanStatus>) -> PlanStatus {
    let total = record.items.len();
    if total == 0 {
        return fallback.unwrap_or(record.status);
    }

    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut running = 0usize;
    let mut pending = 0usize;

    for item in &record.items {
        match item.status {
            PlanItemStatus::Completed | PlanItemStatus::Skipped => completed += 1,
            PlanItemStatus::Failed | PlanItemStatus::Cancelled => failed += 1,
            PlanItemStatus::Running => running += 1,
            PlanItemStatus::Pending => pending += 1,
        }
    }

    if failed > 0 && completed > 0 {
        return PlanStatus::PartiallyCompleted;
    }
    if failed > 0 && running == 0 && pending == 0 {
        return PlanStatus::Failed;
    }
    if completed == total {
        return PlanStatus::Completed;
    }
    if running > 0 || completed > 0 {
        return PlanStatus::Executing;
    }
    fallback.unwrap_or(record.status)
}

fn compute_item_progress(item: &PlanItem) -> f64 {
    if item.task_ids.is_empty() {
        if item.status == PlanItemStatus::Completed {
            return 100.0;
        }
        if matches!(
            item.status,
            PlanItemStatus::Failed | PlanItemStatus::Cancelled
        ) {
            return item.progress.max(1.0);
        }
        return item.progress;
    }
    let terminal = ["completed", "failed", "skipped", "cancelled"];
    let done_count = item
        .task_ids
        .iter()
        .filter(|tid| {
            let s = item
                .task_statuses
                .get(*tid)
                .map(|s| s.as_str())
                .unwrap_or("pending");
            terminal.contains(&s)
        })
        .count();
    let pct = (done_count as f64 / item.task_ids.len() as f64 * 100.0).round();
    pct.min(100.0)
}

fn compute_item_status(item: &PlanItem) -> PlanItemStatus {
    let statuses: Vec<&str> = item
        .task_ids
        .iter()
        .map(|tid| {
            item.task_statuses
                .get(tid)
                .map(|s| s.as_str())
                .unwrap_or("pending")
        })
        .collect();
    if statuses.is_empty() {
        return item.status;
    }
    if statuses.iter().any(|s| *s == "failed") {
        return PlanItemStatus::Failed;
    }
    if statuses
        .iter()
        .all(|s| *s == "completed" || *s == "skipped" || *s == "cancelled")
    {
        return PlanItemStatus::Completed;
    }
    if statuses
        .iter()
        .any(|s| *s == "in_progress" || *s == "running")
    {
        return PlanItemStatus::Running;
    }
    PlanItemStatus::Pending
}

fn compute_acceptance_summary(criteria: &[AcceptanceCriterion]) -> PlanAcceptanceSummary {
    if criteria.is_empty() {
        return PlanAcceptanceSummary::Pending;
    }
    let met = criteria.iter().filter(|c| c.met).count();
    if met == criteria.len() {
        PlanAcceptanceSummary::Passed
    } else if met > 0 {
        PlanAcceptanceSummary::Partial
    } else {
        PlanAcceptanceSummary::Pending
    }
}

fn normalize_acceptance_criteria(input: Option<Vec<String>>) -> Vec<AcceptanceCriterion> {
    let Some(items) = input else {
        return Vec::new();
    };
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|description| AcceptanceCriterion {
            description,
            met: false,
        })
        .collect()
}

fn normalize_string_vec(input: Option<Vec<String>>) -> Vec<String> {
    let Some(items) = input else {
        return Vec::new();
    };
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn create_initial_runtime_state(
    acceptance_criteria: Vec<AcceptanceCriterion>,
    now: u64,
) -> PlanRuntimeState {
    PlanRuntimeState {
        acceptance: PlanRuntimeAcceptance {
            criteria: acceptance_criteria,
            summary: PlanAcceptanceSummary::Pending,
            updated_at: now,
        },
        review: PlanRuntimeReviewState {
            round: 0,
            state: ReviewState::Idle,
            last_reviewed_at: None,
        },
        replan: PlanRuntimeReplanState {
            state: ReplanState::None,
            reason: None,
            updated_at: None,
        },
        wait: PlanRuntimeWaitState {
            state: WaitState::None,
            reason_code: None,
            updated_at: None,
        },
        phase: PlanRuntimePhaseState {
            state: PhaseState::Idle,
            current_index: None,
            current_title: None,
            next_index: None,
            next_title: None,
            remaining_phases: Vec::new(),
            continuation_intent: ContinuationIntent::Continue,
            updated_at: None,
        },
        termination: PlanRuntimeTerminationState::default(),
    }
}

fn build_prompt_digest(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.len() <= 128 {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..128])
    }
}

fn normalize_attempt_input(input: &PlanAttemptStartInput) -> NormalizedAttemptSelector {
    let assignment_id = input.assignment_id.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let task_id = input.task_id.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let explicit_target = input.target_id.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let target_id = explicit_target
        .or_else(|| {
            if input.scope == PlanAttemptScope::Task {
                task_id.clone()
            } else {
                None
            }
        })
        .or_else(|| {
            if input.scope == PlanAttemptScope::Assignment {
                assignment_id.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "orchestrator".to_string());

    NormalizedAttemptSelector {
        scope: input.scope,
        target_id,
        assignment_id,
        task_id,
        reason: input.reason.clone(),
    }
}

fn normalize_attempt_input_from_complete(input: &PlanAttemptCompleteInput) -> NormalizedAttemptSelector {
    let assignment_id = input.assignment_id.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let task_id = input.task_id.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let explicit_target = input.target_id.as_deref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let target_id = explicit_target
        .or_else(|| {
            if input.scope == PlanAttemptScope::Task {
                task_id.clone()
            } else {
                None
            }
        })
        .or_else(|| {
            if input.scope == PlanAttemptScope::Assignment {
                assignment_id.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "orchestrator".to_string());

    NormalizedAttemptSelector {
        scope: input.scope,
        target_id,
        assignment_id,
        task_id,
        reason: None,
    }
}

fn find_latest_attempt_mut<'a>(
    attempts: &'a mut [PlanAttemptRecord],
    selector: &NormalizedAttemptSelector,
    statuses: Option<&[PlanAttemptStatus]>,
) -> Option<&'a mut PlanAttemptRecord> {
    let mut best_idx: Option<usize> = None;
    let mut best_seq = 0u32;
    let mut best_updated = 0u64;

    for (idx, attempt) in attempts.iter().enumerate() {
        if attempt.scope != selector.scope || attempt.target_id != selector.target_id {
            continue;
        }
        if let Some(aid) = &selector.assignment_id {
            if attempt.assignment_id.as_deref() != Some(aid) {
                continue;
            }
        }
        if let Some(tid) = &selector.task_id {
            if attempt.task_id.as_deref() != Some(tid) {
                continue;
            }
        }
        if let Some(allowed) = statuses {
            if !allowed.contains(&attempt.status) {
                continue;
            }
        }
        if best_idx.is_none()
            || attempt.sequence > best_seq
            || (attempt.sequence == best_seq && attempt.updated_at > best_updated)
        {
            best_idx = Some(idx);
            best_seq = attempt.sequence;
            best_updated = attempt.updated_at;
        }
    }

    best_idx.map(|idx| &mut attempts[idx])
}

fn next_attempt_sequence(
    attempts: &[PlanAttemptRecord],
    selector: &NormalizedAttemptSelector,
) -> u32 {
    let mut max = 0u32;
    for attempt in attempts {
        if attempt.scope != selector.scope || attempt.target_id != selector.target_id {
            continue;
        }
        if let Some(aid) = &selector.assignment_id {
            if attempt.assignment_id.as_deref() != Some(aid) {
                continue;
            }
        }
        if let Some(tid) = &selector.task_id {
            if attempt.task_id.as_deref() != Some(tid) {
                continue;
            }
        }
        if attempt.sequence > max {
            max = attempt.sequence;
        }
    }
    max + 1
}

fn generate_plan_id(now: u64) -> String {
    let seq = PLAN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("plan-{now}-{seq}")
}

fn generate_attempt_id(scope: &PlanAttemptScope, target_id: &str, sequence: u32) -> String {
    let safe_target: String = target_id
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(48)
        .collect();
    let safe_target = if safe_target.is_empty() {
        "target".to_string()
    } else {
        safe_target
    };
    let now = now_millis();
    format!("attempt-{scope:?}-{safe_target}-{sequence}-{now}")
}

fn add_unique(vec: &mut Vec<String>, value: &str) {
    if !vec.iter().any(|v| v == value) {
        vec.push(value.to_string());
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
