import type { CanonicalTurn, CanonicalTurnItem } from './canonical-turn';
import { isCanonicalTerminalStatus, isMainlineCanonicalTurn } from './canonical-turn';
import type { ProcessingStateSnapshot, TurnStage } from './processing-state';
import type { OrchestratorRuntimeState } from '../../types/message';

export function selectLatestMainlineCanonicalTurn(
  turns: readonly CanonicalTurn[],
  sessionId: string,
): CanonicalTurn | null {
  let latest: CanonicalTurn | null = null;
  for (const turn of turns) {
    if (turn.sessionId !== sessionId || !isMainlineCanonicalTurn(turn)) continue;
    if (!latest || turn.turnSeq > latest.turnSeq
      || (turn.turnSeq === latest.turnSeq && turn.acceptedAt > latest.acceptedAt)) {
      latest = turn;
    }
  }
  return latest;
}

/** 启动快照和实时事件共用的主线展示投影，不从代理、任务或本地提交推断状态。 */
export function deriveMainlineRuntimeState(
  turns: readonly CanonicalTurn[],
  sessionId: string,
): Pick<OrchestratorRuntimeState, 'turnId' | 'status' | 'phase' | 'startedAt' | 'endedAt' | 'statusChangedAt' | 'lastEventAt'> | null {
  const turn = selectLatestMainlineCanonicalTurn(turns, sessionId);
  if (!turn) return null;
  const status = turn.status === 'pending' ? 'running'
    : turn.status === 'interrupted' ? 'failed'
      : turn.status === 'superseded' ? 'cancelled'
        : turn.status;
  const updatedAt = turn.items.reduce((latest, item) => Math.max(latest, item.updatedAt),
    Math.max(turn.acceptedAt, turn.completedAt ?? 0));
  return {
    turnId: turn.turnId,
    status,
    phase: status === 'running' ? deriveTurnStage(turn) : status,
    startedAt: turn.acceptedAt,
    endedAt: turn.completedAt,
    statusChangedAt: updatedAt,
    lastEventAt: updatedAt,
  };
}

function hasActiveItem(items: readonly CanonicalTurnItem[]): boolean {
  return items.some((item) => !isCanonicalTerminalStatus(item.status));
}

function hasResponseActivity(items: readonly CanonicalTurnItem[]): boolean {
  return items.some((item) => item.kind !== 'user_message');
}

function isFinalizingTurn(turn: CanonicalTurn): boolean {
  if (hasActiveItem(turn.items)) {
    return false;
  }
  const lastItem = turn.items.at(-1);
  return Boolean(
    lastItem
    && lastItem.kind === 'assistant_text'
    && isCanonicalTerminalStatus(lastItem.status),
  );
}

function deriveTurnStage(turn: CanonicalTurn): TurnStage {
  if (isCanonicalTerminalStatus(turn.status)) {
    return 'done';
  }
  if (turn.status === 'pending') {
    return 'pending';
  }
  if (!hasResponseActivity(turn.items)) {
    return 'preparing';
  }
  return isFinalizingTurn(turn) ? 'finalizing' : 'streaming';
}

function readMetadataString(
  metadata: Record<string, unknown> | undefined,
  key: string,
): string {
  const value = metadata?.[key];
  return typeof value === 'string' ? value.trim() : '';
}

export function canonicalTurnRequestId(turn: CanonicalTurn): string {
  const turnRequestId = readMetadataString(turn.metadata, 'requestId')
    || readMetadataString(turn.metadata, 'request_id');
  if (turnRequestId) {
    return turnRequestId;
  }

  const rootUserItem = turn.items
    .filter((item) => item.kind === 'user_message')
    .sort((left, right) => left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId))[0];
  return readMetadataString(rootUserItem?.metadata, 'requestId')
    || readMetadataString(rootUserItem?.metadata, 'request_id');
}

export function deriveProcessingStateFromCanonicalTurns(
  canonicalTurns: CanonicalTurn[],
  sessionId: string,
): ProcessingStateSnapshot | null {
  if (!sessionId) {
    return null;
  }
  const activeTurns = canonicalTurns
    .filter((turn) => (
      turn.sessionId === sessionId
      && isMainlineCanonicalTurn(turn)
      && !isCanonicalTerminalStatus(turn.status)
    ))
    .sort((left, right) => left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId));
  if (activeTurns.length === 0) {
    return null;
  }

  const pendingRequestIds = new Set<string>();
  let startedAt = Number.POSITIVE_INFINITY;
  let stage: TurnStage | null = null;
  for (const turn of activeTurns) {
    startedAt = Math.min(startedAt, turn.acceptedAt);
    stage = deriveTurnStage(turn);
    const requestId = canonicalTurnRequestId(turn);
    if (requestId) {
      pendingRequestIds.add(requestId);
    }
  }

  return {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: Number.isFinite(startedAt) ? startedAt : 0,
    pendingRequestIds: [...pendingRequestIds],
    stage,
  };
}
