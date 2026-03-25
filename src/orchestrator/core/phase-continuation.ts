import type { PlanRuntimePhaseState } from '../plan-ledger';

export function hasPhaseContinuationPending(
  phase?: PlanRuntimePhaseState | null,
): boolean {
  if (!phase) {
    return false;
  }
  return phase.state === 'awaiting_next_phase'
    && phase.continuationIntent === 'continue'
    && Array.isArray(phase.remainingPhases)
    && phase.remainingPhases.length > 0;
}

export function buildPhaseContinuationSignature(
  phase?: PlanRuntimePhaseState | null,
): string {
  if (!phase) {
    return '';
  }
  const remainingCount = Array.isArray(phase.remainingPhases) ? phase.remainingPhases.length : 0;
  const currentIndex = typeof phase.currentIndex === 'number' && Number.isFinite(phase.currentIndex)
    ? phase.currentIndex
    : 'na';
  const nextIndex = typeof phase.nextIndex === 'number' && Number.isFinite(phase.nextIndex)
    ? phase.nextIndex
    : 'na';

  if (
    phase.state === 'idle'
    && phase.continuationIntent === 'stop'
    && remainingCount === 0
    && currentIndex === 'na'
    && nextIndex === 'na'
  ) {
    return '';
  }

  return [
    `state:${phase.state}`,
    `intent:${phase.continuationIntent}`,
    `current:${currentIndex}`,
    `next:${nextIndex}`,
    `remaining:${remainingCount}`,
  ].join('|');
}
