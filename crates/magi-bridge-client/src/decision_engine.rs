use crate::orchestrator_termination::{
    OrchestratorTerminationReason, TerminationCandidate, TerminationSnapshot,
};

#[derive(Clone, Debug)]
pub struct OrchestratorExecutionBudget {
    pub max_duration_ms: u64,
    pub max_token_usage: u64,
    pub max_error_rate: f64,
    pub max_rounds: u32,
}

#[derive(Clone, Debug)]
pub struct OrchestratorDecisionPolicy {
    pub stalled_window_size: u32,
    pub external_wait_sla_ms: u64,
    pub upstream_model_error_streak: u32,
    pub error_rate_min_samples: u32,
    pub budget_no_progress_streak_threshold: u32,
    pub budget_breach_streak_threshold: u32,
    pub external_wait_breach_streak_threshold: u32,
    pub budget_hard_limit_factor: f64,
    pub external_wait_hard_limit_factor: f64,
}

#[derive(Clone, Debug, Default)]
pub struct OrchestratorGateState {
    pub no_progress_streak: u32,
    pub consecutive_upstream_model_errors: u32,
    pub budget_breach_streak: u32,
    pub external_wait_breach_streak: u32,
}

#[derive(Clone, Debug)]
pub struct OrchestratorGateEvent {
    pub gate: GateKind,
    pub hard: bool,
    pub label: String,
    pub payload: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateKind {
    Budget,
    ExternalWait,
    UpstreamModelError,
    Stalled,
}

pub struct BudgetCandidatesResult {
    pub candidates: Vec<TerminationCandidate>,
    pub events: Vec<OrchestratorGateEvent>,
}

pub struct GateStreakResult {
    pub budget_breach_streak: u32,
    pub external_wait_breach_streak: u32,
}

pub struct OrchestratorDecisionEngine {
    policy: OrchestratorDecisionPolicy,
}

impl OrchestratorDecisionEngine {
    pub fn new(policy: OrchestratorDecisionPolicy) -> Self {
        Self { policy }
    }

    pub fn update_gate_streaks(
        &self,
        snapshot: &TerminationSnapshot,
        budget: &OrchestratorExecutionBudget,
        no_progress_streak: u32,
        current_budget_breach_streak: u32,
        current_external_wait_breach_streak: u32,
    ) -> GateStreakResult {
        let running_required = snapshot.running_required.unwrap_or(0);
        if running_required > 0 {
            return GateStreakResult {
                budget_breach_streak: 0,
                external_wait_breach_streak: 0,
            };
        }

        let budget_gate_armed = self.is_budget_gate_armed(no_progress_streak);
        let budget_breach_streak =
            if budget_gate_armed && self.is_budget_threshold_breached(snapshot, budget) {
                current_budget_breach_streak + 1
            } else {
                0
            };

        let external_wait_breach_streak =
            if self.is_external_wait_threshold_breached(snapshot) {
                current_external_wait_breach_streak + 1
            } else {
                0
            };

        GateStreakResult {
            budget_breach_streak,
            external_wait_breach_streak,
        }
    }

    pub fn collect_budget_candidates<F>(
        &self,
        snapshot: &TerminationSnapshot,
        budget: &OrchestratorExecutionBudget,
        gate_state: &OrchestratorGateState,
        create_candidate: F,
    ) -> BudgetCandidatesResult
    where
        F: Fn(OrchestratorTerminationReason, &str) -> TerminationCandidate,
    {
        let mut candidates = Vec::new();
        let mut events = Vec::new();
        let running_required = snapshot.running_required.unwrap_or(0);

        if snapshot.required_total == 0 {
            return BudgetCandidatesResult {
                candidates,
                events,
            };
        }

        if running_required == 0 {
            let hard_budget_breach = self.is_hard_budget_breach(snapshot, budget);
            let budget_gate_armed =
                hard_budget_breach || self.is_budget_gate_armed(gate_state.no_progress_streak);
            if budget_gate_armed
                && (hard_budget_breach
                    || gate_state.budget_breach_streak
                        >= self.policy.budget_breach_streak_threshold)
            {
                let label = if hard_budget_breach {
                    "budget_hard"
                } else {
                    "budget_debounced"
                };
                candidates.push(create_candidate(
                    OrchestratorTerminationReason::BudgetExceeded,
                    label,
                ));
                events.push(OrchestratorGateEvent {
                    gate: GateKind::Budget,
                    hard: hard_budget_breach,
                    label: label.to_string(),
                    payload: serde_json::json!({
                        "requiredTotal": snapshot.required_total,
                        "attemptSeq": snapshot.attempt_seq,
                        "budgetBreachStreak": gate_state.budget_breach_streak,
                        "elapsedMs": snapshot.budget_state.elapsed_ms,
                        "tokenUsed": snapshot.budget_state.token_used,
                    }),
                });
            }

            let hard_external_wait_breach = self.is_hard_external_wait_breach(snapshot);
            if hard_external_wait_breach
                || gate_state.external_wait_breach_streak
                    >= self.policy.external_wait_breach_streak_threshold
            {
                let label = if hard_external_wait_breach {
                    "external_wait_hard"
                } else {
                    "external_wait_debounced"
                };
                candidates.push(create_candidate(
                    OrchestratorTerminationReason::ExternalWaitTimeout,
                    label,
                ));
                events.push(OrchestratorGateEvent {
                    gate: GateKind::ExternalWait,
                    hard: hard_external_wait_breach,
                    label: label.to_string(),
                    payload: serde_json::json!({
                        "requiredTotal": snapshot.required_total,
                        "attemptSeq": snapshot.attempt_seq,
                        "externalWaitBreachStreak": gate_state.external_wait_breach_streak,
                        "maxExternalWaitAgeMs": snapshot.blocker_state.max_external_wait_age_ms,
                    }),
                });
            }
        }

        if gate_state.consecutive_upstream_model_errors
            >= self.policy.upstream_model_error_streak
        {
            candidates.push(create_candidate(
                OrchestratorTerminationReason::UpstreamModelError,
                "upstream_model",
            ));
            events.push(OrchestratorGateEvent {
                gate: GateKind::UpstreamModelError,
                hard: false,
                label: "upstream_model".to_string(),
                payload: serde_json::json!({
                    "requiredTotal": snapshot.required_total,
                    "attemptSeq": snapshot.attempt_seq,
                    "consecutiveUpstreamModelErrors": gate_state.consecutive_upstream_model_errors,
                }),
            });
        }

        if snapshot.required_total > 0
            && gate_state.no_progress_streak >= self.policy.stalled_window_size
            && snapshot.blocker_state.external_wait_open == 0
            && running_required == 0
        {
            candidates.push(create_candidate(
                OrchestratorTerminationReason::Stalled,
                "stalled",
            ));
            events.push(OrchestratorGateEvent {
                gate: GateKind::Stalled,
                hard: false,
                label: "stalled".to_string(),
                payload: serde_json::json!({
                    "requiredTotal": snapshot.required_total,
                    "attemptSeq": snapshot.attempt_seq,
                    "noProgressStreak": gate_state.no_progress_streak,
                    "unresolvedBlockers": snapshot.progress_vector.unresolved_blockers,
                }),
            });
        }

        BudgetCandidatesResult {
            candidates,
            events,
        }
    }

    pub fn resolve_shadow_reason(
        &self,
        snapshot: &TerminationSnapshot,
        budget: &OrchestratorExecutionBudget,
        gate_state: &OrchestratorGateState,
        assistant_text: &str,
    ) -> OrchestratorTerminationReason {
        let use_task_track_guards = snapshot.required_total > 0;
        let running_required = snapshot.running_required.unwrap_or(0);

        if snapshot.required_total > 0
            && snapshot.progress_vector.terminal_required_tasks >= snapshot.required_total
            && snapshot.running_or_pending_required == 0
        {
            return if snapshot.failed_required > 0 {
                OrchestratorTerminationReason::Failed
            } else {
                OrchestratorTerminationReason::Completed
            };
        }

        if use_task_track_guards && running_required == 0 {
            if self.is_hard_budget_breach(snapshot, budget)
                || (self.is_budget_gate_armed(gate_state.no_progress_streak)
                    && gate_state.budget_breach_streak
                        >= self.policy.budget_breach_streak_threshold)
            {
                return OrchestratorTerminationReason::BudgetExceeded;
            }
            if self.is_hard_external_wait_breach(snapshot)
                || gate_state.external_wait_breach_streak
                    >= self.policy.external_wait_breach_streak_threshold
            {
                return OrchestratorTerminationReason::ExternalWaitTimeout;
            }
        }

        if use_task_track_guards
            && gate_state.consecutive_upstream_model_errors
                >= self.policy.upstream_model_error_streak
        {
            return OrchestratorTerminationReason::UpstreamModelError;
        }

        if snapshot.required_total > 0
            && gate_state.no_progress_streak >= self.policy.stalled_window_size
            && snapshot.blocker_state.external_wait_open == 0
            && running_required == 0
        {
            return OrchestratorTerminationReason::Stalled;
        }

        if assistant_text.trim().is_empty() {
            return OrchestratorTerminationReason::Failed;
        }

        OrchestratorTerminationReason::Completed
    }

    pub fn is_budget_threshold_breached(
        &self,
        snapshot: &TerminationSnapshot,
        budget: &OrchestratorExecutionBudget,
    ) -> bool {
        if snapshot.required_total == 0 {
            return false;
        }
        if snapshot.running_required.unwrap_or(0) > 0 {
            return false;
        }
        snapshot.budget_state.elapsed_ms >= budget.max_duration_ms
            || snapshot.budget_state.token_used >= budget.max_token_usage
            || self.is_error_rate_budget_exceeded(snapshot, budget)
    }

    pub fn is_external_wait_threshold_breached(&self, snapshot: &TerminationSnapshot) -> bool {
        if snapshot.required_total == 0 {
            return false;
        }
        if snapshot.running_required.unwrap_or(0) > 0 {
            return false;
        }
        snapshot.blocker_state.max_external_wait_age_ms >= self.policy.external_wait_sla_ms
    }

    pub fn is_hard_budget_breach(
        &self,
        snapshot: &TerminationSnapshot,
        budget: &OrchestratorExecutionBudget,
    ) -> bool {
        if snapshot.required_total == 0 {
            return false;
        }
        if snapshot.running_required.unwrap_or(0) > 0 {
            return false;
        }
        let dur_limit =
            (budget.max_duration_ms as f64 * self.policy.budget_hard_limit_factor).ceil() as u64;
        let tok_limit =
            (budget.max_token_usage as f64 * self.policy.budget_hard_limit_factor).ceil() as u64;
        snapshot.budget_state.elapsed_ms >= dur_limit
            || snapshot.budget_state.token_used >= tok_limit
    }

    pub fn is_hard_external_wait_breach(&self, snapshot: &TerminationSnapshot) -> bool {
        if snapshot.required_total == 0 {
            return false;
        }
        if snapshot.running_required.unwrap_or(0) > 0 {
            return false;
        }
        let limit = (self.policy.external_wait_sla_ms as f64
            * self.policy.external_wait_hard_limit_factor)
            .ceil() as u64;
        snapshot.blocker_state.max_external_wait_age_ms >= limit
    }

    fn is_error_rate_budget_exceeded(
        &self,
        snapshot: &TerminationSnapshot,
        budget: &OrchestratorExecutionBudget,
    ) -> bool {
        if snapshot.required_total == 0 {
            return false;
        }
        if snapshot.attempt_seq < self.policy.error_rate_min_samples {
            return false;
        }
        snapshot.budget_state.error_rate >= budget.max_error_rate
    }

    fn is_budget_gate_armed(&self, no_progress_streak: u32) -> bool {
        no_progress_streak >= self.policy.budget_no_progress_streak_threshold
    }
}
