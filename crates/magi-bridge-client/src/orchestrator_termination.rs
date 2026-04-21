use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorTerminationReason {
    Completed,
    Failed,
    Cancelled,
    GovernancePause,
    Stalled,
    BudgetExceeded,
    ExternalWaitTimeout,
    ExternalAbort,
    UpstreamModelError,
    Interrupted,
    Unknown,
}

pub type KnownTerminationReason = OrchestratorTerminationReason;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressVector {
    pub terminal_required_todos: u32,
    pub accepted_criteria: u32,
    pub critical_path_resolved: u32,
    pub unresolved_blockers: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewState {
    pub accepted: u32,
    pub total: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockerState {
    pub open: u32,
    pub score: f64,
    pub external_wait_open: u32,
    pub max_external_wait_age_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetState {
    pub elapsed_ms: u64,
    pub token_used: u64,
    pub remaining_tokens: Option<u64>,
    pub token_limit: Option<u64>,
    pub usage_ratio: Option<f64>,
    pub warning_level: Option<String>,
    pub error_rate: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheState {
    pub mode: Option<String>,
    pub health: Option<String>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cache_read_ratio: Option<f64>,
    pub baseline_cache_read_tokens: Option<u64>,
    pub suspected_break: Option<bool>,
    pub last_break_reason: Option<String>,
    pub last_reset_at: Option<u64>,
    pub last_reset_reason: Option<String>,
    pub last_observed_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminationSnapshot {
    pub snapshot_id: String,
    pub plan_id: String,
    pub attempt_seq: u32,
    pub progress_vector: ProgressVector,
    pub review_state: ReviewState,
    pub blocker_state: BlockerState,
    pub budget_state: BudgetState,
    pub cache_state: Option<CacheState>,
    pub cp_version: u32,
    pub required_total: u32,
    pub failed_required: u32,
    pub running_or_pending_required: u32,
    pub running_required: Option<u32>,
    pub source_event_ids: Vec<String>,
    pub computed_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminationCandidate {
    pub reason: OrchestratorTerminationReason,
    pub event_id: String,
    pub triggered_at: u64,
}

fn termination_priority(reason: OrchestratorTerminationReason) -> u32 {
    match reason {
        OrchestratorTerminationReason::Cancelled => 1,
        OrchestratorTerminationReason::ExternalAbort => 2,
        OrchestratorTerminationReason::Interrupted => 2,
        OrchestratorTerminationReason::GovernancePause => 3,
        OrchestratorTerminationReason::BudgetExceeded => 3,
        OrchestratorTerminationReason::ExternalWaitTimeout => 4,
        OrchestratorTerminationReason::UpstreamModelError => 5,
        OrchestratorTerminationReason::Failed => 6,
        OrchestratorTerminationReason::Stalled => 7,
        OrchestratorTerminationReason::Completed => 8,
        OrchestratorTerminationReason::Unknown => 999,
    }
}

pub struct TerminationResolution {
    pub reason: OrchestratorTerminationReason,
    pub evidence_ids: Vec<String>,
}

pub fn resolve_termination_reason(
    candidates: &[TerminationCandidate],
    fallback: OrchestratorTerminationReason,
) -> TerminationResolution {
    if candidates.is_empty() {
        return TerminationResolution {
            reason: fallback,
            evidence_ids: Vec::new(),
        };
    }

    let mut sorted: Vec<&TerminationCandidate> = candidates.iter().collect();
    sorted.sort_by(|a, b| {
        let pa = termination_priority(a.reason);
        let pb = termination_priority(b.reason);
        pa.cmp(&pb)
            .then_with(|| a.triggered_at.cmp(&b.triggered_at))
    });

    let reason = sorted[0].reason;
    let evidence_ids: Vec<String> = sorted
        .iter()
        .filter(|c| c.reason == reason)
        .map(|c| c.event_id.clone())
        .collect();

    TerminationResolution {
        reason,
        evidence_ids,
    }
}

pub struct ProgressEvaluation {
    pub progressed: bool,
    pub regressed: bool,
}

pub fn evaluate_progress(
    prev: Option<&TerminationSnapshot>,
    curr: &TerminationSnapshot,
) -> ProgressEvaluation {
    let prev = match prev {
        Some(p) => p,
        None => {
            return ProgressEvaluation {
                progressed: true,
                regressed: false,
            }
        }
    };

    if prev.cp_version != curr.cp_version {
        return ProgressEvaluation {
            progressed: true,
            regressed: false,
        };
    }

    let p0 = &prev.progress_vector;
    let p1 = &curr.progress_vector;

    let improved = p1.terminal_required_todos > p0.terminal_required_todos
        || p1.accepted_criteria > p0.accepted_criteria
        || p1.critical_path_resolved > p0.critical_path_resolved
        || p1.unresolved_blockers < p0.unresolved_blockers;

    let regressed = p1.terminal_required_todos < p0.terminal_required_todos
        || p1.accepted_criteria < p0.accepted_criteria
        || p1.critical_path_resolved < p0.critical_path_resolved
        || p1.unresolved_blockers > p0.unresolved_blockers;

    ProgressEvaluation {
        progressed: improved && !regressed,
        regressed,
    }
}
