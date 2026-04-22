use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationDecision {
    Run,
    PauseForSystem,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationRunKind {
    NextPhase,
    TokenBudget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetWarningLevel {
    Normal,
    Notice,
    Warning,
    Danger,
}

const TOKEN_BUDGET_CONTINUATION_MIN_REMAINING: u64 = 1500;
const TOKEN_BUDGET_CONTINUATION_MAX_USAGE_RATIO: f64 = 0.85;
const DIMINISHING_RETURNS_THRESHOLD_TOKENS: u64 = 500;
const DIMINISHING_RETURNS_MIN_ROUNDS: usize = 2;

#[derive(Clone, Debug, Default)]
pub struct BudgetState {
    pub remaining_tokens: Option<u64>,
    pub token_limit: Option<u64>,
    pub usage_ratio: Option<f64>,
    pub warning_level: Option<BudgetWarningLevel>,
    pub continuation_round_output_deltas: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct ContinuationDecisionInput {
    pub allow_deep_continuation: bool,
    pub is_governance_paused: bool,
    pub has_phase_continuation_pending: bool,
    pub pending_required_tasks: usize,
    pub has_meaningful_progress: bool,
    pub budget_state: Option<BudgetState>,
}

#[derive(Clone, Debug)]
pub struct ContinuationDecisionResult {
    pub decision: ContinuationDecision,
    pub run_kind: Option<ContinuationRunKind>,
    pub rationale: Vec<String>,
}

pub fn decide_continuation_action(input: &ContinuationDecisionInput) -> ContinuationDecisionResult {
    let mut rationale = Vec::new();

    if input.is_governance_paused {
        rationale.push("continuation:pause_for_system".to_string());
        return ContinuationDecisionResult {
            decision: ContinuationDecision::PauseForSystem,
            run_kind: None,
            rationale,
        };
    }

    if input.allow_deep_continuation && input.has_phase_continuation_pending {
        rationale.push("continuation:run_next_phase".to_string());
        return ContinuationDecisionResult {
            decision: ContinuationDecision::Run,
            run_kind: Some(ContinuationRunKind::NextPhase),
            rationale,
        };
    }

    let pending = input.pending_required_tasks;
    if input.allow_deep_continuation
        && pending > 0
        && input.has_meaningful_progress
        && has_budget_continuation_capacity(input.budget_state.as_ref(), pending)
    {
        let deltas = input
            .budget_state
            .as_ref()
            .map(|b| &b.continuation_round_output_deltas[..])
            .unwrap_or(&[]);
        if is_diminishing_returns(deltas, pending) {
            rationale.push("continuation:stop_diminishing_returns".to_string());
            return ContinuationDecisionResult {
                decision: ContinuationDecision::Stop,
                run_kind: None,
                rationale,
            };
        }

        rationale.push("continuation:run_token_budget".to_string());
        return ContinuationDecisionResult {
            decision: ContinuationDecision::Run,
            run_kind: Some(ContinuationRunKind::TokenBudget),
            rationale,
        };
    }

    rationale.push("continuation:stop".to_string());
    ContinuationDecisionResult {
        decision: ContinuationDecision::Stop,
        run_kind: None,
        rationale,
    }
}

fn has_budget_continuation_capacity(budget: Option<&BudgetState>, pending: usize) -> bool {
    let Some(budget) = budget else {
        return false;
    };
    if matches!(
        budget.warning_level,
        Some(BudgetWarningLevel::Warning) | Some(BudgetWarningLevel::Danger)
    ) {
        return false;
    }
    let remaining = budget.remaining_tokens.unwrap_or(0);
    let usage_ratio = budget.usage_ratio.unwrap_or(1.0).clamp(0.0, 1.0);
    let thresholds = resolve_budget_continuation_thresholds(pending);

    remaining >= thresholds.min_remaining_tokens && usage_ratio < thresholds.max_usage_ratio
}

fn is_diminishing_returns(deltas: &[u64], pending: usize) -> bool {
    let rounds_window = if pending >= 4 {
        3
    } else {
        DIMINISHING_RETURNS_MIN_ROUNDS
    };
    if deltas.len() < rounds_window {
        return false;
    }
    let low_output_threshold = if pending <= 1 {
        300
    } else if pending >= 4 {
        700
    } else {
        DIMINISHING_RETURNS_THRESHOLD_TOKENS
    };
    deltas[deltas.len() - rounds_window..]
        .iter()
        .all(|&d| d < low_output_threshold)
}

struct BudgetThresholds {
    min_remaining_tokens: u64,
    max_usage_ratio: f64,
}

fn resolve_budget_continuation_thresholds(pending: usize) -> BudgetThresholds {
    let pressure = pending.clamp(1, 6) as u64;
    let min_remaining = if pressure <= 1 {
        TOKEN_BUDGET_CONTINUATION_MIN_REMAINING
    } else {
        (TOKEN_BUDGET_CONTINUATION_MIN_REMAINING + (pressure - 1) * 300).min(3000)
    };
    let max_ratio = if pressure >= 5 {
        0.9
    } else if pressure >= 3 {
        0.88
    } else if pressure == 2 {
        0.86
    } else {
        TOKEN_BUDGET_CONTINUATION_MAX_USAGE_RATIO
    };
    BudgetThresholds {
        min_remaining_tokens: min_remaining,
        max_usage_ratio: max_ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> ContinuationDecisionInput {
        ContinuationDecisionInput {
            allow_deep_continuation: true,
            is_governance_paused: false,
            has_phase_continuation_pending: false,
            pending_required_tasks: 0,
            has_meaningful_progress: true,
            budget_state: None,
        }
    }

    #[test]
    fn governance_paused_returns_pause() {
        let mut input = base_input();
        input.is_governance_paused = true;
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::PauseForSystem);
    }

    #[test]
    fn phase_continuation_pending_runs_next_phase() {
        let mut input = base_input();
        input.has_phase_continuation_pending = true;
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Run);
        assert_eq!(result.run_kind, Some(ContinuationRunKind::NextPhase));
    }

    #[test]
    fn no_pending_tasks_stops() {
        let input = base_input();
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Stop);
    }

    #[test]
    fn pending_tasks_with_budget_runs() {
        let mut input = base_input();
        input.pending_required_tasks = 2;
        input.budget_state = Some(BudgetState {
            remaining_tokens: Some(5000),
            usage_ratio: Some(0.5),
            warning_level: Some(BudgetWarningLevel::Normal),
            ..Default::default()
        });
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Run);
        assert_eq!(result.run_kind, Some(ContinuationRunKind::TokenBudget));
    }

    #[test]
    fn danger_warning_stops() {
        let mut input = base_input();
        input.pending_required_tasks = 2;
        input.budget_state = Some(BudgetState {
            remaining_tokens: Some(5000),
            usage_ratio: Some(0.5),
            warning_level: Some(BudgetWarningLevel::Danger),
            ..Default::default()
        });
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Stop);
    }

    #[test]
    fn diminishing_returns_stops() {
        let mut input = base_input();
        input.pending_required_tasks = 2;
        input.budget_state = Some(BudgetState {
            remaining_tokens: Some(5000),
            usage_ratio: Some(0.5),
            warning_level: Some(BudgetWarningLevel::Normal),
            continuation_round_output_deltas: vec![100, 200],
            ..Default::default()
        });
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Stop);
        assert!(result.rationale.iter().any(|r| r.contains("diminishing")));
    }

    #[test]
    fn no_budget_state_stops() {
        let mut input = base_input();
        input.pending_required_tasks = 3;
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Stop);
    }

    #[test]
    fn not_deep_stops() {
        let mut input = base_input();
        input.allow_deep_continuation = false;
        input.pending_required_tasks = 3;
        input.budget_state = Some(BudgetState {
            remaining_tokens: Some(5000),
            usage_ratio: Some(0.5),
            warning_level: Some(BudgetWarningLevel::Normal),
            ..Default::default()
        });
        let result = decide_continuation_action(&input);
        assert_eq!(result.decision, ContinuationDecision::Stop);
    }
}
