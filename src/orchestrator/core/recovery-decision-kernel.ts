import type { OrchestratorTerminationReason } from '../../llm/adapters/orchestrator-termination';

type ResolvedOrchestratorTerminationReason = Exclude<OrchestratorTerminationReason, 'unknown'>;

export interface RecoveryRuntimeSnapshot {
  reviewState?: {
    accepted?: number;
    total?: number;
  };
  blockerState?: {
    open?: number;
    externalWaitOpen?: number;
    maxExternalWaitAgeMs?: number;
  };
  budgetState?: {
    errorRate?: number;
  };
  requiredTotal?: number;
  failedRequired?: number;
  runningOrPendingRequired?: number;
}

export interface RecoveryAuditOutcome {
  issues?: Array<{ dimension?: string; detail?: string; level?: string }>;
}

export interface ReplanGateSignals {
  budgetPressure: boolean;
  scopeExpansion: boolean;
  scopeIssues: string[];
  acceptanceFailure: boolean;
  blockerPressure: boolean;
  progressStalled: boolean;
  pendingRequiredTodos: number;
  failedRequiredTodos: number;
  unresolvedBlockers: number;
  externalWaitOpen: number;
}

export type ReplanSource =
  | 'delivery_failed'
  | 'ask_followup_pending'
  | 'budget_pressure'
  | 'scope_expansion'
  | 'acceptance_failure'
  | 'blocker_pressure'
  | 'progress_stalled';

export type RecoveryDecisionAction =
  | 'none'
  | 'auto_repair'
  | 'auto_repair_stalled_notice'
  | 'auto_governance_resume'
  | 'pause';

interface BaseRecoveryDecisionInput {
  allowAutoGovernanceResume: boolean;
  isGovernancePaused: boolean;
  governanceReason?: ResolvedOrchestratorTerminationReason;
  governanceRecoveryAttempt: number;
  governanceRecoveryMaxRounds: number;
  deliveryFailed: boolean;
  continuationPolicy?: 'auto' | 'stop';
  canAutoRepairByRounds: boolean;
  autoRepairStalled: boolean;
}

export interface RecoveryDecisionResult {
  action: RecoveryDecisionAction;
  replanSource?: ReplanSource;
  rationale: string[];
}

function toFiniteInt(value: unknown, fallback = 0): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return fallback;
  }
  return Math.max(0, Math.floor(value));
}

export function isGovernanceAutoRecoverReason(
  reason?: ResolvedOrchestratorTerminationReason,
): boolean {
  return reason === 'upstream_model_error' || reason === 'external_wait_timeout';
}

export function deriveReplanGateSignals(input: {
  runtimeReason?: ResolvedOrchestratorTerminationReason;
  runtimeSnapshot?: RecoveryRuntimeSnapshot;
  auditOutcome?: RecoveryAuditOutcome | null;
}): ReplanGateSignals {
  const runtimeReason = input.runtimeReason;
  const snapshot = input.runtimeSnapshot;
  const budgetState = snapshot?.budgetState;
  const blockerState = snapshot?.blockerState;
  const reviewState = snapshot?.reviewState;

  const pendingRequiredTodos = toFiniteInt(snapshot?.runningOrPendingRequired);
  const failedRequiredTodos = toFiniteInt(snapshot?.failedRequired);
  const unresolvedBlockers = toFiniteInt(blockerState?.open);
  const externalWaitOpen = toFiniteInt(blockerState?.externalWaitOpen);
  const reviewAccepted = toFiniteInt(reviewState?.accepted);
  const reviewTotal = toFiniteInt(reviewState?.total);

  const budgetPressureByReason = runtimeReason === 'budget_exceeded';
  const budgetPressureByErrorRate = typeof budgetState?.errorRate === 'number'
    && Number.isFinite(budgetState.errorRate)
    && budgetState.errorRate >= 0.5;
  const budgetPressure = budgetPressureByReason || budgetPressureByErrorRate;

  const scopeIssues = (input.auditOutcome?.issues || [])
    .filter((issue) => issue?.dimension === 'scope')
    .map((issue) => (typeof issue.detail === 'string' ? issue.detail.trim() : ''))
    .filter((detail) => detail.length > 0);

  const acceptanceFailureByFailedTodos = failedRequiredTodos > 0;
  const acceptanceFailureByReview = reviewTotal > 0 && reviewAccepted < reviewTotal && runtimeReason === 'failed';
  const acceptanceFailure = acceptanceFailureByFailedTodos || acceptanceFailureByReview;

  const blockerPressure = unresolvedBlockers > 0
    || externalWaitOpen > 0
    || runtimeReason === 'external_wait_timeout';

  const progressStalled = runtimeReason === 'stalled'
    || (pendingRequiredTodos > 0 && unresolvedBlockers > 0);

  return {
    budgetPressure,
    scopeExpansion: scopeIssues.length > 0,
    scopeIssues,
    acceptanceFailure,
    blockerPressure,
    progressStalled,
    pendingRequiredTodos,
    failedRequiredTodos,
    unresolvedBlockers,
    externalWaitOpen,
  };
}

function decideRecoveryAction(input: BaseRecoveryDecisionInput): RecoveryDecisionResult {
  const rationale: string[] = [];

  if (
    input.deliveryFailed
    && input.continuationPolicy === 'auto'
    && input.canAutoRepairByRounds
    && !input.autoRepairStalled
    && !input.isGovernancePaused
  ) {
    rationale.push('delivery_failed:auto_repair');
    return { action: 'auto_repair', rationale };
  }

  if (
    input.deliveryFailed
    && input.continuationPolicy === 'auto'
    && input.autoRepairStalled
  ) {
    rationale.push('delivery_failed:auto_repair_stalled');
    return { action: 'auto_repair_stalled_notice', rationale };
  }

  if (
    input.isGovernancePaused
    && input.allowAutoGovernanceResume
    && isGovernanceAutoRecoverReason(input.governanceReason)
    && input.governanceRecoveryAttempt < input.governanceRecoveryMaxRounds
  ) {
    rationale.push('governance:auto_resume');
    return { action: 'auto_governance_resume', rationale };
  }

  if (input.isGovernancePaused) {
    rationale.push('governance:pause');
    return { action: 'pause', rationale };
  }

  return { action: 'none', rationale };
}

// ---- 阶段化入口 ----
// 外层循环 3 次调用 decideRecoveryAction 的职责不同，
// 以下入口函数显式约束每个阶段的参数边界，替代调用侧的硬编码遮蔽。

/** 阶段①：交付修复决策——只关心 delivery repair 和 governance pause */
export interface DeliveryRecoveryInput extends BaseRecoveryDecisionInput {
}
export function decideDeliveryRecovery(input: DeliveryRecoveryInput): RecoveryDecisionResult {
  return decideRecoveryAction(input);
}

/** 阶段②：治理恢复决策——只关心 governance resume */
export type GovernanceRecoveryInput = Omit<BaseRecoveryDecisionInput, 'deliveryFailed'>;
export function decideGovernanceRecovery(input: GovernanceRecoveryInput): RecoveryDecisionResult {
  return decideRecoveryAction({
    ...input,
    deliveryFailed: false,
  });
}
