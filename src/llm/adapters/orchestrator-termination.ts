export type OrchestratorTerminationReason =
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'stalled'
  | 'budget_exceeded'
  | 'external_wait_timeout'
  | 'external_abort'
  | 'upstream_model_error'
  | 'interrupted'
  | 'unknown';

export interface ProgressVector {
  terminalRequiredTodos: number;
  acceptedCriteria: number;
  criticalPathResolved: number;
  unresolvedBlockers: number;
}

export interface TerminationSnapshot {
  snapshotId: string;
  planId: string;
  attemptSeq: number;
  progressVector: ProgressVector;
  reviewState: {
    accepted: number;
    total: number;
  };
  blockerState: {
    open: number;
    score: number;
    externalWaitOpen: number;
    maxExternalWaitAgeMs: number;
  };
  budgetState: {
    elapsedMs: number;
    tokenUsed: number;
    errorRate: number;
  };
  cpVersion: number;
  requiredTotal: number;
  failedRequired: number;
  runningOrPendingRequired: number;
  runningRequired?: number;
  sourceEventIds: string[];
  computedAt: number;
}

export interface TerminationCandidate {
  reason: Exclude<OrchestratorTerminationReason, 'unknown'>;
  eventId: string;
  triggeredAt: number;
}

const TERMINATION_PRIORITY: Record<Exclude<OrchestratorTerminationReason, 'unknown'>, number> = {
  cancelled: 1,
  external_abort: 2,
  budget_exceeded: 3,
  external_wait_timeout: 4,
  upstream_model_error: 5,
  failed: 6,
  stalled: 7,
  completed: 8,
  interrupted: 2,
};

export function resolveTerminationReason(
  candidates: TerminationCandidate[],
  fallback: Exclude<OrchestratorTerminationReason, 'unknown'> = 'completed'
): { reason: Exclude<OrchestratorTerminationReason, 'unknown'>; evidenceIds: string[] } {
  if (!candidates || candidates.length === 0) {
    return { reason: fallback, evidenceIds: [] };
  }

  const sorted = [...candidates].sort((a, b) => {
    const pa = TERMINATION_PRIORITY[a.reason] ?? 999;
    const pb = TERMINATION_PRIORITY[b.reason] ?? 999;
    if (pa !== pb) {
      return pa - pb;
    }
    if (a.triggeredAt !== b.triggeredAt) {
      return a.triggeredAt - b.triggeredAt;
    }
    return a.reason.localeCompare(b.reason);
  });

  const reason = sorted[0]?.reason || fallback;
  const evidenceIds = sorted
    .filter((item) => item.reason === reason)
    .map((item) => item.eventId);

  return { reason, evidenceIds };
}

export function evaluateProgress(
  prev: TerminationSnapshot | null,
  curr: TerminationSnapshot,
  epsilon = 1e-6
): { progressed: boolean; regressed: boolean } {
  if (!prev) {
    return { progressed: true, regressed: false };
  }

  if (prev.cpVersion !== curr.cpVersion) {
    return { progressed: true, regressed: false };
  }

  const p0 = prev.progressVector;
  const p1 = curr.progressVector;

  const improved =
    p1.terminalRequiredTodos > p0.terminalRequiredTodos
    || p1.acceptedCriteria > p0.acceptedCriteria
    || p1.criticalPathResolved > p0.criticalPathResolved + epsilon
    || p1.unresolvedBlockers < p0.unresolvedBlockers;

  const regressed =
    p1.terminalRequiredTodos < p0.terminalRequiredTodos
    || p1.acceptedCriteria < p0.acceptedCriteria
    || p1.criticalPathResolved + epsilon < p0.criticalPathResolved
    || p1.unresolvedBlockers > p0.unresolvedBlockers;

  return { progressed: improved && !regressed, regressed };
}
