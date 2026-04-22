use serde::{Deserialize, Serialize};

use super::entry_router::PlanMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestComplexity {
    Simple,
    Complex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorWritePolicy {
    Allowed,
    Limited,
    Forbidden,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorBudget {
    pub max_duration_ms: u64,
    pub max_token_usage: u64,
    pub max_error_rate: f64,
    pub max_rounds: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceProfile {
    pub mode: PlanMode,
    pub complexity: RequestComplexity,
    pub orchestrator_budget: OrchestratorBudget,
    pub orchestrator_write_policy: OrchestratorWritePolicy,
    pub no_progress_streak_threshold: u32,
    pub worker_rounds_multiplier: u32,
    pub total_recovery_rounds_limit: u32,
}

const BASE_BUDGET: OrchestratorBudget = OrchestratorBudget {
    max_duration_ms: 420_000,
    max_token_usage: 120_000,
    max_error_rate: 0.7,
    max_rounds: 30,
};

const DEEP_DURATION_MULTIPLIER: f64 = 900_000.0 / 420_000.0;
const DEEP_TOKEN_MULTIPLIER: f64 = 280_000.0 / 120_000.0;
const DEEP_ROUNDS_MULTIPLIER: f64 = 80.0 / 30.0;
const DEEP_ERROR_RATE_BONUS: f64 = 0.1;
const DEEP_WORKER_ROUNDS: u32 = 3;
const DEEP_NO_PROGRESS_STREAK: u32 = 3;
const DEEP_RECOVERY_ROUNDS: u32 = 20;

const STANDARD_NO_PROGRESS_STREAK: u32 = 2;
const STANDARD_WORKER_ROUNDS_MULTIPLIER: u32 = 1;
const STANDARD_RECOVERY_ROUNDS: u32 = 10;

const DEEP_SIMPLE_DISCOUNT: f64 = 0.6;

pub fn resolve_governance_profile(
    mode: PlanMode,
    complexity: RequestComplexity,
) -> GovernanceProfile {
    match (mode, complexity) {
        (PlanMode::Deep, RequestComplexity::Simple) => build_deep_simple_profile(),
        (PlanMode::Deep, RequestComplexity::Complex) => build_deep_complex_profile(),
        (PlanMode::Standard, _) => build_standard_profile(complexity),
    }
}

pub fn resolve_orchestrator_budget(mode: PlanMode) -> OrchestratorBudget {
    if mode == PlanMode::Deep {
        build_deep_budget()
    } else {
        BASE_BUDGET.clone()
    }
}

pub fn resolve_no_progress_streak_threshold(mode: PlanMode) -> u32 {
    if mode == PlanMode::Deep {
        DEEP_NO_PROGRESS_STREAK
    } else {
        STANDARD_NO_PROGRESS_STREAK
    }
}

fn build_deep_budget() -> OrchestratorBudget {
    OrchestratorBudget {
        max_duration_ms: (BASE_BUDGET.max_duration_ms as f64 * DEEP_DURATION_MULTIPLIER).round()
            as u64,
        max_token_usage: (BASE_BUDGET.max_token_usage as f64 * DEEP_TOKEN_MULTIPLIER).round()
            as u64,
        max_error_rate: (BASE_BUDGET.max_error_rate + DEEP_ERROR_RATE_BONUS).min(1.0),
        max_rounds: (BASE_BUDGET.max_rounds as f64 * DEEP_ROUNDS_MULTIPLIER).round() as u32,
    }
}

fn build_deep_simple_budget() -> OrchestratorBudget {
    let deep = build_deep_budget();
    let base = &BASE_BUDGET;
    OrchestratorBudget {
        max_duration_ms: (base.max_duration_ms as f64
            + (deep.max_duration_ms - base.max_duration_ms) as f64 * DEEP_SIMPLE_DISCOUNT)
            .round() as u64,
        max_token_usage: (base.max_token_usage as f64
            + (deep.max_token_usage - base.max_token_usage) as f64 * DEEP_SIMPLE_DISCOUNT)
            .round() as u64,
        max_error_rate: base.max_error_rate
            + (deep.max_error_rate - base.max_error_rate) * DEEP_SIMPLE_DISCOUNT,
        max_rounds: ((base.max_rounds as f64)
            + (deep.max_rounds - base.max_rounds) as f64 * DEEP_SIMPLE_DISCOUNT)
            .round() as u32,
    }
}

fn build_standard_profile(complexity: RequestComplexity) -> GovernanceProfile {
    GovernanceProfile {
        mode: PlanMode::Standard,
        complexity,
        orchestrator_budget: BASE_BUDGET.clone(),
        orchestrator_write_policy: OrchestratorWritePolicy::Allowed,
        no_progress_streak_threshold: STANDARD_NO_PROGRESS_STREAK,
        worker_rounds_multiplier: STANDARD_WORKER_ROUNDS_MULTIPLIER,
        total_recovery_rounds_limit: STANDARD_RECOVERY_ROUNDS,
    }
}

fn build_deep_simple_profile() -> GovernanceProfile {
    GovernanceProfile {
        mode: PlanMode::Deep,
        complexity: RequestComplexity::Simple,
        orchestrator_budget: build_deep_simple_budget(),
        orchestrator_write_policy: OrchestratorWritePolicy::Limited,
        no_progress_streak_threshold: DEEP_NO_PROGRESS_STREAK,
        worker_rounds_multiplier: 2,
        total_recovery_rounds_limit: (DEEP_RECOVERY_ROUNDS as f64 * DEEP_SIMPLE_DISCOUNT).round()
            as u32,
    }
}

fn build_deep_complex_profile() -> GovernanceProfile {
    GovernanceProfile {
        mode: PlanMode::Deep,
        complexity: RequestComplexity::Complex,
        orchestrator_budget: build_deep_budget(),
        orchestrator_write_policy: OrchestratorWritePolicy::Forbidden,
        no_progress_streak_threshold: DEEP_NO_PROGRESS_STREAK,
        worker_rounds_multiplier: DEEP_WORKER_ROUNDS,
        total_recovery_rounds_limit: DEEP_RECOVERY_ROUNDS,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_simple_uses_base_budget() {
        let profile = resolve_governance_profile(PlanMode::Standard, RequestComplexity::Simple);
        assert_eq!(profile.orchestrator_budget.max_rounds, 30);
        assert_eq!(
            profile.orchestrator_write_policy,
            OrchestratorWritePolicy::Allowed
        );
        assert_eq!(profile.worker_rounds_multiplier, 1);
    }

    #[test]
    fn standard_complex_same_budget() {
        let profile = resolve_governance_profile(PlanMode::Standard, RequestComplexity::Complex);
        assert_eq!(profile.orchestrator_budget.max_rounds, 30);
        assert_eq!(
            profile.orchestrator_write_policy,
            OrchestratorWritePolicy::Allowed
        );
    }

    #[test]
    fn deep_complex_forbidden_write() {
        let profile = resolve_governance_profile(PlanMode::Deep, RequestComplexity::Complex);
        assert_eq!(
            profile.orchestrator_write_policy,
            OrchestratorWritePolicy::Forbidden
        );
        assert_eq!(profile.worker_rounds_multiplier, 3);
        assert_eq!(profile.orchestrator_budget.max_rounds, 80);
        assert_eq!(profile.total_recovery_rounds_limit, 20);
    }

    #[test]
    fn deep_simple_limited_write() {
        let profile = resolve_governance_profile(PlanMode::Deep, RequestComplexity::Simple);
        assert_eq!(
            profile.orchestrator_write_policy,
            OrchestratorWritePolicy::Limited
        );
        assert_eq!(profile.worker_rounds_multiplier, 2);
        assert!(profile.orchestrator_budget.max_rounds > 30);
        assert!(profile.orchestrator_budget.max_rounds < 80);
    }

    #[test]
    fn deep_simple_budget_between_standard_and_deep() {
        let standard =
            resolve_governance_profile(PlanMode::Standard, RequestComplexity::Simple);
        let deep_simple =
            resolve_governance_profile(PlanMode::Deep, RequestComplexity::Simple);
        let deep_complex =
            resolve_governance_profile(PlanMode::Deep, RequestComplexity::Complex);

        assert!(
            deep_simple.orchestrator_budget.max_token_usage
                > standard.orchestrator_budget.max_token_usage
        );
        assert!(
            deep_simple.orchestrator_budget.max_token_usage
                < deep_complex.orchestrator_budget.max_token_usage
        );
    }

    #[test]
    fn resolve_budget_standard() {
        let budget = resolve_orchestrator_budget(PlanMode::Standard);
        assert_eq!(budget.max_rounds, 30);
        assert_eq!(budget.max_duration_ms, 420_000);
    }

    #[test]
    fn resolve_budget_deep() {
        let budget = resolve_orchestrator_budget(PlanMode::Deep);
        assert_eq!(budget.max_rounds, 80);
    }

    #[test]
    fn no_progress_streak_standard() {
        assert_eq!(
            resolve_no_progress_streak_threshold(PlanMode::Standard),
            2
        );
    }

    #[test]
    fn no_progress_streak_deep() {
        assert_eq!(resolve_no_progress_streak_threshold(PlanMode::Deep), 3);
    }
}
