import type {
  CanonicalTurn,
  CanonicalTurnEvent,
} from '../shared/protocol/canonical-turn';
import type { SessionTimelineProjection } from '../types/message';
import {
  createCanonicalTurnReducerState,
  reduceCanonicalTurnEvent,
  replaceCanonicalTurns,
} from './turn-reducer';
import { buildCanonicalTimelineProjection } from './turn-projection';

export const turnStoreState = $state({
  reducer: createCanonicalTurnReducerState(''),
  projection: null as SessionTimelineProjection | null,
  lastError: null as string | null,
});

function normalizeSessionId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function publishProjection(): SessionTimelineProjection | null {
  turnStoreState.projection = buildCanonicalTimelineProjection(turnStoreState.reducer);
  return turnStoreState.projection;
}

export function applyCanonicalTurnEvent(event: CanonicalTurnEvent): SessionTimelineProjection | null {
  const result = reduceCanonicalTurnEvent(turnStoreState.reducer, event);
  if (result.error) {
    turnStoreState.lastError = result.error;
    console.error('[canonical-turn-store] 拒绝 canonical turn event:', result.error);
    return null;
  }
  if (!result.changed) {
    return turnStoreState.projection;
  }
  turnStoreState.lastError = null;
  turnStoreState.reducer = result.state;
  return publishProjection();
}

export function replaceCanonicalSessionTurns(sessionId: string, turns: CanonicalTurn[]): SessionTimelineProjection | null {
  turnStoreState.reducer = replaceCanonicalTurns(sessionId, turns);
  turnStoreState.lastError = null;
  return publishProjection();
}

export function clearCanonicalSessionTurns(sessionId?: string): void {
  const nextSessionId = normalizeSessionId(sessionId);
  turnStoreState.reducer = createCanonicalTurnReducerState(nextSessionId);
  turnStoreState.projection = null;
  turnStoreState.lastError = null;
}

export { buildCanonicalTimelineProjection };
