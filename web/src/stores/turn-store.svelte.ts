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
import { buildCanonicalTimelineProjection, updateCanonicalTimelineProjection } from './turn-projection';

export const turnStoreState = $state({
  reducer: createCanonicalTurnReducerState(''),
  projection: null as SessionTimelineProjection | null,
  lastError: null as string | null,
});

const domPaintScheduledTurnIds = new Set<string>();

function browserTimingTraceId(event: CanonicalTurnEvent): string {
  const metadata = event.turn?.metadata || event.item?.metadata;
  const requestId = metadata?.requestId;
  return typeof requestId === 'string' && requestId.trim()
    ? requestId.trim()
    : event.turnId;
}

function markBrowserTiming(stage: string, event: CanonicalTurnEvent, startedAt: number): void {
  const viteEnv = (import.meta as ImportMeta & { env?: { DEV?: boolean } }).env;
  if (!viteEnv?.DEV || typeof performance === 'undefined') return;
  const elapsed = Math.max(0, performance.now() - startedAt);
  performance.mark(`magi-${stage}-${event.turnId}-${Math.round(performance.now())}`);
  console.debug('[magi.performance]', {
    traceId: browserTimingTraceId(event),
    turnId: event.turnId,
    stage,
    elapsedMs: Number(elapsed.toFixed(2)),
  });
}

function normalizeSessionId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function publishProjection(): SessionTimelineProjection | null {
  turnStoreState.projection = buildCanonicalTimelineProjection(turnStoreState.reducer);
  return turnStoreState.projection;
}

export function applyCanonicalTurnEvent(event: CanonicalTurnEvent): SessionTimelineProjection | null {
  const receivedAt = typeof performance === 'undefined' ? 0 : performance.now();
  markBrowserTiming('event_received', event, receivedAt);
  const result = reduceCanonicalTurnEvent(turnStoreState.reducer, event);
  markBrowserTiming('reducer_completed', event, receivedAt);
  if (result.error) {
    turnStoreState.lastError = result.error;
    console.error('[canonical-turn-store] 拒绝 canonical turn event:', result.error);
    return null;
  }
  if (result.recoveryRequired) {
    // 增量发生基线缺口时，当前内容不能被错误拼接。状态仍已推进，随后由调用方
    // 拉取权威快照补齐；这属于可恢复的同步问题，不应污染浏览器错误日志。
    turnStoreState.reducer = result.state;
    turnStoreState.lastError = 'canonical stream baseline requires recovery';
    return null;
  }
  if (!result.changed) {
    if (result.cursorAdvanced) {
      turnStoreState.reducer = result.state;
    }
    return turnStoreState.projection;
  }
  turnStoreState.lastError = null;
  turnStoreState.reducer = result.state;
  turnStoreState.projection = updateCanonicalTimelineProjection(
    turnStoreState.projection,
    turnStoreState.reducer,
    result.changedTurnIds,
  );
  markBrowserTiming('projection_completed', event, receivedAt);
  if (result.changed && !domPaintScheduledTurnIds.has(event.turnId)) {
    domPaintScheduledTurnIds.add(event.turnId);
    const onPaint = () => {
      domPaintScheduledTurnIds.delete(event.turnId);
      markBrowserTiming('dom_painted', event, receivedAt);
    };
    if (typeof requestAnimationFrame === 'function') requestAnimationFrame(onPaint);
    else setTimeout(onPaint, 0);
  }
  return turnStoreState.projection;
}

export function replaceCanonicalSessionTurns(
  sessionId: string,
  turns: CanonicalTurn[],
  lastAppliedEventSeq = 0,
): SessionTimelineProjection | null {
  turnStoreState.reducer = replaceCanonicalTurns(sessionId, turns, lastAppliedEventSeq);
  turnStoreState.lastError = null;
  return publishProjection();
}

export function prependCanonicalSessionTurns(
  sessionId: string,
  turns: CanonicalTurn[],
): SessionTimelineProjection | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId || turnStoreState.reducer.sessionId !== normalizedSessionId) {
    return turnStoreState.projection;
  }
  const byTurnId = new Map(turns
    .filter((turn) => turn.sessionId === normalizedSessionId)
    .map((turn) => [turn.turnId, turn] as const));
  for (const turn of turnStoreState.reducer.turns) {
    byTurnId.set(turn.turnId, turn);
  }
  turnStoreState.reducer = {
    ...turnStoreState.reducer,
    turns: [...byTurnId.values()]
      .map((turn) => ({
        ...turn,
        items: turn.items.map((item) => ({
          ...item,
          blocks: Array.isArray(item.blocks) ? [...item.blocks] : undefined,
          tool: item.tool ? { ...item.tool } : undefined,
          worker: item.worker ? { ...item.worker } : undefined,
          visibility: { ...item.visibility },
          metadata: item.metadata ? { ...item.metadata } : undefined,
        })),
      }))
      .sort((left, right) => left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId)),
  };
  turnStoreState.lastError = null;
  return publishProjection();
}

export function clearCanonicalSessionTurns(sessionId?: string): void {
  const nextSessionId = normalizeSessionId(sessionId);
  turnStoreState.reducer = createCanonicalTurnReducerState(nextSessionId);
  turnStoreState.projection = null;
  turnStoreState.lastError = null;
}

export { buildCanonicalTimelineProjection, updateCanonicalTimelineProjection };
