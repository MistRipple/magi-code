import {
  type OrchestratorTerminationReason,
  type TerminationCandidate,
  type TerminationSnapshot,
} from './orchestrator-termination';

export interface OrchestratorExecutionBudget {
  maxDurationMs: number;
  maxTokenUsage: number;
  maxErrorRate: number;
  /** 工具循环最大轮次硬限——条件收敛失败时的最后防线 */
  maxRounds: number;
}

export interface OrchestratorDecisionPolicy {
  stalledWindowSize: number;
  externalWaitSlaMs: number;
  upstreamModelErrorStreak: number;
  errorRateMinSamples: number;
  budgetNoProgressStreakThreshold: number;
  budgetBreachStreakThreshold: number;
  externalWaitBreachStreakThreshold: number;
  budgetHardLimitFactor: number;
  externalWaitHardLimitFactor: number;
}

export interface OrchestratorGateState {
  noProgressStreak: number;
  consecutiveUpstreamModelErrors: number;
  budgetBreachStreak: number;
  externalWaitBreachStreak: number;
}

export interface OrchestratorGateEvent {
  gate: 'budget' | 'external_wait' | 'upstream_model_error' | 'stalled';
  hard: boolean;
  label: string;
  payload: Record<string, unknown>;
}

type KnownTerminationReason = Exclude<OrchestratorTerminationReason, 'unknown'>;

export class OrchestratorDecisionEngine {
  constructor(private readonly policy: OrchestratorDecisionPolicy) {}

  public updateGateStreaks(params: {
    snapshot: TerminationSnapshot;
    budget: OrchestratorExecutionBudget;
    noProgressStreak: number;
    current: Pick<OrchestratorGateState, 'budgetBreachStreak' | 'externalWaitBreachStreak'>;
  }): Pick<OrchestratorGateState, 'budgetBreachStreak' | 'externalWaitBreachStreak'> {
    const { snapshot, budget, noProgressStreak, current } = params;
    const runningRequired = snapshot.runningRequired ?? 0;
    if (runningRequired > 0) {
      return { budgetBreachStreak: 0, externalWaitBreachStreak: 0 };
    }
    const budgetGateArmed = this.isBudgetGateArmed(noProgressStreak);
    const budgetBreachStreak = budgetGateArmed && this.isBudgetThresholdBreached(snapshot, budget)
      ? current.budgetBreachStreak + 1
      : 0;
    const externalWaitBreachStreak = this.isExternalWaitThresholdBreached(snapshot)
      ? current.externalWaitBreachStreak + 1
      : 0;
    return { budgetBreachStreak, externalWaitBreachStreak };
  }

  public collectBudgetCandidates(params: {
    snapshot: TerminationSnapshot;
    budget: OrchestratorExecutionBudget;
    gateState: OrchestratorGateState;
    createCandidate: (reason: KnownTerminationReason, label: string) => TerminationCandidate;
  }): { candidates: TerminationCandidate[]; events: OrchestratorGateEvent[] } {
    const { snapshot, budget, gateState, createCandidate } = params;
    const { noProgressStreak, consecutiveUpstreamModelErrors, budgetBreachStreak, externalWaitBreachStreak } = gateState;
    const candidates: TerminationCandidate[] = [];
    const events: OrchestratorGateEvent[] = [];
    const runningRequired = snapshot.runningRequired ?? 0;

    // 仅 required todo 轨道启用预算/超时/上游错误硬门禁。
    if (snapshot.requiredTotal === 0) {
      return { candidates, events };
    }

    if (runningRequired === 0) {
      const hardBudgetBreach = this.isHardBudgetBreach(snapshot, budget);
      const budgetGateArmed = hardBudgetBreach || this.isBudgetGateArmed(noProgressStreak);
      if (budgetGateArmed && (hardBudgetBreach || budgetBreachStreak >= this.policy.budgetBreachStreakThreshold)) {
        const label = hardBudgetBreach ? 'budget_hard' : 'budget_debounced';
        candidates.push(createCandidate('budget_exceeded', label));
        events.push({
          gate: 'budget',
          hard: hardBudgetBreach,
          label,
          payload: {
            requiredTotal: snapshot.requiredTotal,
            attemptSeq: snapshot.attemptSeq,
            budgetBreachStreak,
            elapsedMs: snapshot.budgetState.elapsedMs,
            tokenUsed: snapshot.budgetState.tokenUsed,
          },
        });
      }

      const hardExternalWaitBreach = this.isHardExternalWaitBreach(snapshot);
      if (hardExternalWaitBreach || externalWaitBreachStreak >= this.policy.externalWaitBreachStreakThreshold) {
        const label = hardExternalWaitBreach ? 'external_wait_hard' : 'external_wait_debounced';
        candidates.push(createCandidate('external_wait_timeout', label));
        events.push({
          gate: 'external_wait',
          hard: hardExternalWaitBreach,
          label,
          payload: {
            requiredTotal: snapshot.requiredTotal,
            attemptSeq: snapshot.attemptSeq,
            externalWaitBreachStreak,
            maxExternalWaitAgeMs: snapshot.blockerState.maxExternalWaitAgeMs,
          },
        });
      }
    }

    if (consecutiveUpstreamModelErrors >= this.policy.upstreamModelErrorStreak) {
      const label = 'upstream_model';
      candidates.push(createCandidate('upstream_model_error', label));
      events.push({
        gate: 'upstream_model_error',
        hard: false,
        label,
        payload: {
          requiredTotal: snapshot.requiredTotal,
          attemptSeq: snapshot.attemptSeq,
          consecutiveUpstreamModelErrors,
        },
      });
    }

    if (
      snapshot.requiredTotal > 0
      && noProgressStreak >= this.policy.stalledWindowSize
      && snapshot.blockerState.externalWaitOpen === 0
      && runningRequired === 0
    ) {
      const label = 'stalled';
      candidates.push(createCandidate('stalled', label));
      events.push({
        gate: 'stalled',
        hard: false,
        label,
        payload: {
          requiredTotal: snapshot.requiredTotal,
          attemptSeq: snapshot.attemptSeq,
          noProgressStreak,
          unresolvedBlockers: snapshot.progressVector.unresolvedBlockers,
        },
      });
    }

    return { candidates, events };
  }

  public resolveShadowReason(params: {
    snapshot: TerminationSnapshot;
    budget: OrchestratorExecutionBudget;
    gateState: OrchestratorGateState;
    assistantText: string;
  }): KnownTerminationReason {
    const { snapshot, budget, gateState, assistantText } = params;
    const { noProgressStreak, consecutiveUpstreamModelErrors, budgetBreachStreak, externalWaitBreachStreak } = gateState;
    const useTodoTrackGuards = snapshot.requiredTotal > 0;
    const runningRequired = snapshot.runningRequired ?? 0;

    if (snapshot.requiredTotal > 0
      && snapshot.progressVector.terminalRequiredTodos >= snapshot.requiredTotal
      && snapshot.runningOrPendingRequired === 0) {
      return snapshot.failedRequired > 0 ? 'failed' : 'completed';
    }
    if (useTodoTrackGuards && runningRequired === 0) {
      if (
        this.isHardBudgetBreach(snapshot, budget)
        || (
          this.isBudgetGateArmed(noProgressStreak)
          && budgetBreachStreak >= this.policy.budgetBreachStreakThreshold
        )
      ) {
        return 'budget_exceeded';
      }
      if (
        this.isHardExternalWaitBreach(snapshot)
        || externalWaitBreachStreak >= this.policy.externalWaitBreachStreakThreshold
      ) {
        return 'external_wait_timeout';
      }
    }
    if (
      useTodoTrackGuards
      && consecutiveUpstreamModelErrors >= this.policy.upstreamModelErrorStreak
    ) {
      return 'upstream_model_error';
    }
    if (
      snapshot.requiredTotal > 0
      && noProgressStreak >= this.policy.stalledWindowSize
      && snapshot.blockerState.externalWaitOpen === 0
      && runningRequired === 0
    ) {
      return 'stalled';
    }
    if (!assistantText.trim()) {
      return 'failed';
    }
    return 'completed';
  }

  public isBudgetThresholdBreached(
    snapshot: TerminationSnapshot,
    budget: OrchestratorExecutionBudget,
  ): boolean {
    if (snapshot.requiredTotal === 0) {
      return false;
    }
    if ((snapshot.runningRequired ?? 0) > 0) {
      return false;
    }
    return snapshot.budgetState.elapsedMs >= budget.maxDurationMs
      || snapshot.budgetState.tokenUsed >= budget.maxTokenUsage
      || this.isErrorRateBudgetExceeded(snapshot, budget);
  }

  public isExternalWaitThresholdBreached(snapshot: TerminationSnapshot): boolean {
    if (snapshot.requiredTotal === 0) {
      return false;
    }
    if ((snapshot.runningRequired ?? 0) > 0) {
      return false;
    }
    return snapshot.blockerState.maxExternalWaitAgeMs >= this.policy.externalWaitSlaMs;
  }

  public isHardBudgetBreach(
    snapshot: TerminationSnapshot,
    budget: OrchestratorExecutionBudget,
  ): boolean {
    if (snapshot.requiredTotal === 0) {
      return false;
    }
    if ((snapshot.runningRequired ?? 0) > 0) {
      return false;
    }
    return snapshot.budgetState.elapsedMs >= Math.ceil(
      budget.maxDurationMs * this.policy.budgetHardLimitFactor,
    )
      || snapshot.budgetState.tokenUsed >= Math.ceil(
        budget.maxTokenUsage * this.policy.budgetHardLimitFactor,
      );
  }

  public isHardExternalWaitBreach(snapshot: TerminationSnapshot): boolean {
    if (snapshot.requiredTotal === 0) {
      return false;
    }
    if ((snapshot.runningRequired ?? 0) > 0) {
      return false;
    }
    return snapshot.blockerState.maxExternalWaitAgeMs >= Math.ceil(
      this.policy.externalWaitSlaMs * this.policy.externalWaitHardLimitFactor,
    );
  }

  private isErrorRateBudgetExceeded(
    snapshot: TerminationSnapshot,
    budget: OrchestratorExecutionBudget,
  ): boolean {
    if (snapshot.requiredTotal === 0) {
      return false;
    }
    if (snapshot.attemptSeq < this.policy.errorRateMinSamples) {
      return false;
    }
    return snapshot.budgetState.errorRate >= budget.maxErrorRate;
  }

  private isBudgetGateArmed(noProgressStreak: number): boolean {
    return noProgressStreak >= this.policy.budgetNoProgressStreakThreshold;
  }
}
