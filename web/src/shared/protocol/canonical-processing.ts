import type { UIProcessingState } from '../../types/message';
import type { CanonicalTurn } from './canonical-turn';
import { isCanonicalTerminalStatus } from './canonical-turn';

function readMetadataString(
  metadata: Record<string, unknown> | undefined,
  key: string,
): string {
  const value = metadata?.[key];
  return typeof value === 'string' ? value.trim() : '';
}

function collectTurnRequestIds(turn: CanonicalTurn): string[] {
  const requestIds = new Set<string>();
  const turnRequestId = readMetadataString(turn.metadata, 'requestId');
  if (turnRequestId) {
    requestIds.add(turnRequestId);
  }
  for (const item of turn.items) {
    const itemRequestId = readMetadataString(item.metadata, 'requestId');
    if (itemRequestId) {
      requestIds.add(itemRequestId);
    }
  }
  return [...requestIds];
}

export function deriveProcessingStateFromCanonicalTurns(
  canonicalTurns: CanonicalTurn[],
  sessionId: string,
): UIProcessingState | null {
  if (!sessionId) {
    return null;
  }
  const activeTurns = canonicalTurns
    .filter((turn) => (
      turn.sessionId === sessionId
      && !isCanonicalTerminalStatus(turn.status)
    ))
    .sort((left, right) => left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId));
  if (activeTurns.length === 0) {
    return null;
  }

  const pendingRequestIds = new Set<string>();
  let startedAt = Number.POSITIVE_INFINITY;
  for (const turn of activeTurns) {
    startedAt = Math.min(startedAt, turn.acceptedAt);
    for (const requestId of collectTurnRequestIds(turn)) {
      pendingRequestIds.add(requestId);
    }
  }

  return {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: Number.isFinite(startedAt) ? startedAt : 0,
    pendingRequestIds: [...pendingRequestIds],
  };
}
