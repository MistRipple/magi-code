import type {
  CanonicalTurn,
  CanonicalTurnEvent,
} from '../shared/protocol/canonical-turn';
import { normalizeCanonicalTurnStrict } from '../shared/protocol/canonical-turn';
import type { SessionTimelineProjection } from '../types/message';
import {
  createCanonicalTurnReducerState,
  isLocalOptimisticTurn,
  reduceCanonicalTurnEvent,
  replaceCanonicalTurns,
} from './turn-reducer';
import { canonicalTurnRequestId } from '../shared/protocol/canonical-processing';
import { buildCanonicalTimelineProjection, updateCanonicalTimelineProjection } from './turn-projection';

export const turnStoreState = $state({
  reducer: createCanonicalTurnReducerState(''),
  projection: null as SessionTimelineProjection | null,
  lastError: null as string | null,
});

export interface CanonicalTurnPageWindow {
  expectedBeforeCursor: string | null;
  nextBeforeCursor: string | null;
}

export interface CanonicalTurnPageCommit {
  projection: SessionTimelineProjection;
  addedTurnIds: string[];
  addedTurnCount: number;
  oldestTurnId: string;
  oldestTurnSeq: number;
}

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

export function setCanonicalTimelineError(error: unknown): void {
  const message = error instanceof Error ? error.message.trim() : String(error || '').trim();
  turnStoreState.lastError = message || 'canonical timeline requires recovery';
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

/**
 * 合并同一会话的 bootstrap 窗口。bootstrap 只返回最新一页，不能覆盖此前已经
 * 加载的更早 turn；同 turn 以本次带水位的权威快照为准。
 */
export function mergeCanonicalSessionTurns(
  sessionId: string,
  turns: CanonicalTurn[],
  lastAppliedEventSeq = 0,
): SessionTimelineProjection | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId || turnStoreState.reducer.sessionId !== normalizedSessionId) {
    return turnStoreState.projection;
  }
  const byTurnId = new Map(
    turnStoreState.reducer.turns.map((turn) => [turn.turnId, turn] as const),
  );
  const localOptimisticTurnIdByRequestId = new Map<string, string>();
  for (const turn of turnStoreState.reducer.turns) {
    const requestId = canonicalTurnRequestId(turn);
    if (requestId && isLocalOptimisticTurn(turn)) {
      localOptimisticTurnIdByRequestId.set(requestId, turn.turnId);
    }
  }
  for (const turn of turns) {
    if (turn.sessionId === normalizedSessionId) {
      // HTTP bootstrap 可能比 accepted 事件更晚到达，并携带一个新的后端 turnId。
      // 通过 requestId 将它原子替换本地 optimistic turn，避免同一轮在时间轴中重复，
      // 也避免终态快照只更新了隐藏的 canonical 节点而没有接管可见节点。
      const requestId = canonicalTurnRequestId(turn);
      const localTurnId = requestId ? localOptimisticTurnIdByRequestId.get(requestId) : undefined;
      if (localTurnId && localTurnId !== turn.turnId) {
        byTurnId.delete(localTurnId);
      }
      byTurnId.set(turn.turnId, turn);
    }
  }
  turnStoreState.reducer = {
    ...turnStoreState.reducer,
    lastAppliedEventSeq: Math.max(
      turnStoreState.reducer.lastAppliedEventSeq,
      lastAppliedEventSeq,
    ),
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

export function prependCanonicalSessionTurns(
  sessionId: string,
  turns: CanonicalTurn[],
  pageWindow: CanonicalTurnPageWindow,
): CanonicalTurnPageCommit | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId || turnStoreState.reducer.sessionId !== normalizedSessionId) {
    return null;
  }
  if (!pageWindow || turns.length === 0) {
    return null;
  }

  const expectedBeforeCursor = pageWindow.expectedBeforeCursor?.trim() || null;
  const nextBeforeCursor = pageWindow.nextBeforeCursor?.trim() || null;
  let pageTurns: CanonicalTurn[];
  try {
    pageTurns = turns.map((turn, index) => (
      normalizeCanonicalTurnStrict(turn, `history.canonicalTurns[${index}]`)
    ));
  } catch {
    return null;
  }
  const pageTurnIds = new Set<string>();
  const pageTurnSeqs = new Set<number>();
  const existingTurnSeqs = new Set(
    turnStoreState.reducer.turns.map((turn) => turn.turnSeq),
  );
  for (const turn of pageTurns) {
    if (
      turn.sessionId !== normalizedSessionId
      || pageTurnIds.has(turn.turnId)
      || pageTurnSeqs.has(turn.turnSeq)
      || existingTurnSeqs.has(turn.turnSeq)
    ) {
      return null;
    }
    pageTurnIds.add(turn.turnId);
    pageTurnSeqs.add(turn.turnSeq);
  }

  const existingTurns = [...turnStoreState.reducer.turns].sort(compareCanonicalTurnOrder);
  const currentOldestTurn = existingTurns[0];
  if (currentOldestTurn) {
    if (
      !expectedBeforeCursor
      || expectedBeforeCursor !== currentOldestTurn.turnId
    ) {
      return null;
    }
  } else if (expectedBeforeCursor) {
    return null;
  }

  pageTurns.sort(compareCanonicalTurnOrder);
  const pageOldestTurn = pageTurns[0];
  if (!pageOldestTurn || !nextBeforeCursor || nextBeforeCursor !== pageOldestTurn.turnId) {
    return null;
  }
  if (
    expectedBeforeCursor === nextBeforeCursor
    || (currentOldestTurn && pageTurns.some((turn) => (
      compareCanonicalTurnOrder(turn, currentOldestTurn) >= 0
    )))
    || existingTurns.some((turn) => pageTurnIds.has(turn.turnId))
  ) {
    return null;
  }

  const byTurnId = new Map(existingTurns.map((turn) => [turn.turnId, turn] as const));
  for (const turn of pageTurns) {
    byTurnId.set(turn.turnId, turn);
  }
  const addedTurnIds = pageTurns.map((turn) => turn.turnId);
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
  const projection = publishProjection();
  if (!projection) {
    return null;
  }
  return {
    projection,
    addedTurnIds,
    addedTurnCount: addedTurnIds.length,
    oldestTurnId: pageOldestTurn.turnId,
    oldestTurnSeq: pageOldestTurn.turnSeq,
  };
}

function compareCanonicalTurnOrder(left: CanonicalTurn, right: CanonicalTurn): number {
  return left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId);
}

export function clearCanonicalSessionTurns(sessionId?: string): void {
  const nextSessionId = normalizeSessionId(sessionId);
  turnStoreState.reducer = createCanonicalTurnReducerState(nextSessionId);
  turnStoreState.projection = null;
  turnStoreState.lastError = null;
}

export { buildCanonicalTimelineProjection, updateCanonicalTimelineProjection };
