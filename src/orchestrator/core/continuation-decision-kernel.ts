import type { PlanRuntimePhaseState } from '../plan-ledger';
import { hasPhaseContinuationPending } from './phase-continuation';

export type ContinuationDecision =
  | 'run'
  | 'pause_for_system'
  | 'stop';

export type ContinuationRunKind = 'next_phase';

export interface ContinuationDecisionInput {
  allowDeepContinuation: boolean;
  isGovernancePaused: boolean;
  phaseRuntime?: PlanRuntimePhaseState | null;
}

export interface ContinuationDecisionResult {
  decision: ContinuationDecision;
  runKind?: ContinuationRunKind;
  rationale: string[];
}

export function decideContinuationAction(
  input: ContinuationDecisionInput,
): ContinuationDecisionResult {
  const rationale: string[] = [];

  if (input.isGovernancePaused) {
    rationale.push('continuation:pause_for_system');
    return {
      decision: 'pause_for_system',
      rationale,
    };
  }

  if (input.allowDeepContinuation && hasPhaseContinuationPending(input.phaseRuntime)) {
    rationale.push('continuation:run_next_phase');
    return {
      decision: 'run',
      runKind: 'next_phase',
      rationale,
    };
  }

  rationale.push('continuation:stop');
  return {
    decision: 'stop',
    rationale,
  };
}
