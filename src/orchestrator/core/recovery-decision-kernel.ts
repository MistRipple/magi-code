import type { OrchestratorTerminationReason } from '../../llm/adapters/orchestrator-termination';
import type { InteractionMode } from '../../types';
import type { PlanMode } from '../plan-ledger';

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
  | 'ask_followup_confirmation'
  | 'auto_followup'
  | 'pause';

export interface RecoveryDecisionInput {
  currentPlanMode: PlanMode;
  interactionMode: InteractionMode;
  isGovernancePaused: boolean;
  governanceReason?: ResolvedOrchestratorTerminationReason;
  governanceRecoveryAttempt: number;
  governanceRecoveryMaxRounds: number;
  deliveryFailed: boolean;
  continuationPolicy?: 'auto' | 'ask' | 'stop';
  canAutoRepairByRounds: boolean;
  autoRepairStalled: boolean;
  hasFollowUpPending: boolean;
  followUpSignatureChanged: boolean;
  followUpStallStreak: number;
  blockedFollowUpOnly: boolean;
  signals: ReplanGateSignals;
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

function selectReplanSource(input: {
  hasFollowUpPending: boolean;
  signals: ReplanGateSignals;
}): ReplanSource {
  if (input.hasFollowUpPending) {
    return 'ask_followup_pending';
  }
  if (input.signals.budgetPressure) {
    return 'budget_pressure';
  }
  if (input.signals.scopeExpansion) {
    return 'scope_expansion';
  }
  if (input.signals.acceptanceFailure) {
    return 'acceptance_failure';
  }
  if (input.signals.blockerPressure) {
    return 'blocker_pressure';
  }
  return 'progress_stalled';
}

export function decideRecoveryAction(input: RecoveryDecisionInput): RecoveryDecisionResult {
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
    && input.interactionMode === 'auto'
    && input.currentPlanMode === 'deep'
    && isGovernanceAutoRecoverReason(input.governanceReason)
    && input.governanceRecoveryAttempt < input.governanceRecoveryMaxRounds
  ) {
    rationale.push('governance:auto_resume');
    return { action: 'auto_governance_resume', rationale };
  }

  const shouldAutoFollowUp = input.hasFollowUpPending
    && input.currentPlanMode === 'deep'
    && input.interactionMode === 'auto'
    && input.followUpSignatureChanged
    && input.followUpStallStreak < 2
    && !input.isGovernancePaused
    && !input.blockedFollowUpOnly;

  if (shouldAutoFollowUp) {
    rationale.push('followup:auto_continue');
    return { action: 'auto_followup', rationale };
  }

  const hasGovernanceTrigger = input.signals.budgetPressure
    || input.signals.scopeExpansion
    || input.signals.acceptanceFailure
    || input.signals.blockerPressure
    || input.signals.progressStalled;

  const shouldAskFollowUpConfirmation = input.currentPlanMode === 'deep'
    && input.interactionMode === 'ask'
    && !input.blockedFollowUpOnly
    && !shouldAutoFollowUp
    && (input.hasFollowUpPending || hasGovernanceTrigger)
    && (!input.isGovernancePaused || input.hasFollowUpPending || hasGovernanceTrigger);

  if (shouldAskFollowUpConfirmation) {
    const replanSource = selectReplanSource({
      hasFollowUpPending: input.hasFollowUpPending,
      signals: input.signals,
    });
    rationale.push(`followup:ask_confirmation:${replanSource}`);
    return {
      action: 'ask_followup_confirmation',
      replanSource,
      rationale,
    };
  }

  if (input.isGovernancePaused) {
    rationale.push('governance:pause');
    return { action: 'pause', rationale };
  }

  return { action: 'none', rationale };
}
