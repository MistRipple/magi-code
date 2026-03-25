export interface PlanGovernanceAssessment {
  riskScore: number;
  confidence: number;
  affectedFiles: number;
  crossModules: number;
  writeToolRatio: number;
  historicalFailureRate: number;
  sourceCoverage: number;
  signalAgreement: number;
  historicalCalibration: number;
  decision: 'ask' | 'auto';
  reasons: string[];
}

export interface RuntimeTerminationSnapshot {
  snapshotId?: string;
  progressVector?: {
    terminalRequiredTodos?: number;
    acceptedCriteria?: number;
    criticalPathResolved?: number;
    unresolvedBlockers?: number;
  };
  reviewState?: {
    accepted?: number;
    total?: number;
  };
  blockerState?: {
    open?: number;
    score?: number;
    externalWaitOpen?: number;
    maxExternalWaitAgeMs?: number;
  };
  budgetState?: {
    elapsedMs?: number;
    tokenUsed?: number;
    errorRate?: number;
  };
  requiredTotal?: number;
  failedRequired?: number;
  runningOrPendingRequired?: number;
  runningRequired?: number;
  sourceEventIds?: string[];
}

export interface RuntimeTerminationShadow {
  enabled: boolean;
  reason: string;
  consistent: boolean;
  note?: string;
}

export interface RuntimeTerminationDecisionTraceEntry {
  round?: number;
  phase?: 'no_tool' | 'tool' | 'handoff' | 'finalize';
  action?: 'continue' | 'continue_with_prompt' | 'terminate' | 'handoff' | 'fallback';
  requiredTotal?: number;
  reason?: string;
  candidates?: string[];
  gateState?: {
    noProgressStreak?: number;
    budgetBreachStreak?: number;
    externalWaitBreachStreak?: number;
    consecutiveUpstreamModelErrors?: number;
  };
  note?: string;
  timestamp?: number;
}

export type ResolvedOrchestratorTerminationReason =
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'stalled'
  | 'budget_exceeded'
  | 'external_wait_timeout'
  | 'external_abort'
  | 'upstream_model_error'
  | 'interrupted';
