import type {
  CanonicalTurn,
  CanonicalTurnEvent,
  CanonicalTurnItem,
} from '../shared/protocol/canonical-turn';
import {
  isCanonicalTerminalStatus,
  validateCanonicalTurnItemUpdate,
  validateCanonicalTurnUpdate,
} from '../shared/protocol/canonical-turn';

export interface CanonicalTurnReducerState {
  sessionId: string;
  turns: CanonicalTurn[];
  lastAppliedEventSeq: number;
}

export interface CanonicalTurnReduceResult {
  state: CanonicalTurnReducerState;
  changed: boolean;
  error?: string;
}

export function createCanonicalTurnReducerState(sessionId: string): CanonicalTurnReducerState {
  return {
    sessionId,
    turns: [],
    lastAppliedEventSeq: 0,
  };
}

function normalizeSessionId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function cloneTurnItem(item: CanonicalTurnItem): CanonicalTurnItem {
  return {
    ...item,
    blocks: Array.isArray(item.blocks) ? [...item.blocks] : undefined,
    tool: item.tool ? { ...item.tool } : undefined,
    worker: item.worker ? { ...item.worker } : undefined,
    visibility: {
      ...item.visibility,
      workerTabIds: Array.isArray(item.visibility.workerTabIds)
        ? [...item.visibility.workerTabIds]
        : undefined,
    },
    metadata: item.metadata ? { ...item.metadata } : undefined,
  };
}

function cloneTurn(turn: CanonicalTurn): CanonicalTurn {
  return {
    ...turn,
    items: turn.items.map(cloneTurnItem).sort(compareCanonicalTurnItems),
    metadata: turn.metadata ? { ...turn.metadata } : undefined,
  };
}

function compareCanonicalTurnItems(left: CanonicalTurnItem, right: CanonicalTurnItem): number {
  return left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId);
}

function compareCanonicalTurns(left: CanonicalTurn, right: CanonicalTurn): number {
  return left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId);
}

function mergeCanonicalTurnItem(
  items: CanonicalTurnItem[],
  incoming: CanonicalTurnItem,
): { items: CanonicalTurnItem[]; changed: boolean; error?: string } {
  const existingIndex = items.findIndex((item) => item.itemId === incoming.itemId);
  if (existingIndex < 0) {
    return {
      items: [...items, cloneTurnItem(incoming)].sort(compareCanonicalTurnItems),
      changed: true,
    };
  }

  const existing = items[existingIndex];
  const error = validateCanonicalTurnItemUpdate(existing, incoming);
  if (error) {
    return { items, changed: false, error };
  }

  const next = cloneTurnItem(incoming);
  if (JSON.stringify(existing) === JSON.stringify(next)) {
    return { items, changed: false };
  }

  const merged = [...items];
  merged[existingIndex] = next;
  return {
    items: merged.sort(compareCanonicalTurnItems),
    changed: true,
  };
}

function normalizeLateUpsertItem(
  existingItems: CanonicalTurnItem[],
  incoming: CanonicalTurnItem,
): CanonicalTurnItem {
  const existing = existingItems.find((item) => item.itemId === incoming.itemId);
  if (
    existing
    && isCanonicalTerminalStatus(existing.status)
    && !isCanonicalTerminalStatus(incoming.status)
  ) {
    return cloneTurnItem(existing);
  }
  return incoming;
}

function mergeCanonicalTurn(
  existing: CanonicalTurn | undefined,
  incoming: CanonicalTurn,
): { turn: CanonicalTurn; changed: boolean; error?: string } {
  if (!existing) {
    return { turn: cloneTurn(incoming), changed: true };
  }

  const turnError = validateCanonicalTurnUpdate(existing, incoming);
  if (turnError) {
    return { turn: existing, changed: false, error: turnError };
  }

  let nextItems = existing.items.map(cloneTurnItem);
  let changed = false;
  for (const item of incoming.items) {
    const merged = mergeCanonicalTurnItem(nextItems, item);
    if (merged.error) {
      return { turn: existing, changed: false, error: merged.error };
    }
    nextItems = merged.items;
    changed = changed || merged.changed;
  }

  const nextTurn: CanonicalTurn = {
    ...cloneTurn(existing),
    status: incoming.status,
    completedAt: incoming.completedAt,
    responseDurationMs: incoming.responseDurationMs,
    usage: incoming.usage,
    metadata: incoming.metadata ? { ...incoming.metadata } : existing.metadata,
    items: nextItems,
  };

  if (JSON.stringify(existing) !== JSON.stringify(nextTurn)) {
    changed = true;
  }

  return {
    turn: nextTurn,
    changed,
  };
}

export function replaceCanonicalTurns(
  sessionId: string,
  turns: CanonicalTurn[],
): CanonicalTurnReducerState {
  const normalizedSessionId = normalizeSessionId(sessionId);
  return {
    sessionId: normalizedSessionId,
    turns: turns
      .filter((turn) => turn.sessionId === normalizedSessionId)
      .map(cloneTurn)
      .sort(compareCanonicalTurns),
    lastAppliedEventSeq: 0,
  };
}

export function reduceCanonicalTurnEvent(
  state: CanonicalTurnReducerState,
  event: CanonicalTurnEvent,
): CanonicalTurnReduceResult {
  const normalizedSessionId = normalizeSessionId(event.sessionId);
  if (!normalizedSessionId) {
    return { state, changed: false, error: 'canonical turn event missing sessionId' };
  }
  if (state.sessionId && state.sessionId !== normalizedSessionId) {
    return { state, changed: false, error: `canonical turn event session mismatch: ${state.sessionId} != ${normalizedSessionId}` };
  }
  if (event.eventSeq > 0 && state.lastAppliedEventSeq > 0 && event.eventSeq < state.lastAppliedEventSeq) {
    return { state, changed: false };
  }

  let turns = state.turns.map(cloneTurn);
  let changed = false;
  let targetTurn: CanonicalTurn | undefined = event.turn;
  const existingIndex = turns.findIndex((turn) => turn.turnId === event.turnId);
  const existing = existingIndex >= 0 ? turns[existingIndex] : undefined;

  if (!targetTurn) {
    if (!event.item) {
      return { state, changed: false, error: `canonical turn event ${event.eventId} missing turn and item` };
    }
    targetTurn = existing || {
      sessionId: event.item.sessionId,
      turnId: event.item.turnId,
      turnSeq: event.item.turnSeq,
      acceptedAt: event.item.createdAt,
      status: event.item.status,
      items: [],
    };
  }

  if (
    existing
    && isCanonicalTerminalStatus(existing.status)
    && !isCanonicalTerminalStatus(targetTurn.status)
  ) {
    targetTurn = {
      ...targetTurn,
      status: existing.status,
      completedAt: existing.completedAt,
      responseDurationMs: existing.responseDurationMs,
      usage: existing.usage,
    };
  }

  if (event.kind === 'turn_item_upsert' && existing) {
    targetTurn = {
      ...targetTurn,
      items: targetTurn.items.map((item) => normalizeLateUpsertItem(existing.items, item)),
    };
  }

  let mergedTurn = mergeCanonicalTurn(existing, targetTurn);
  if (mergedTurn.error) {
    return { state, changed: false, error: mergedTurn.error };
  }
  let nextTurn = mergedTurn.turn;
  changed = changed || mergedTurn.changed;

  if (event.item) {
    const incomingItem = event.kind === 'turn_item_upsert'
      ? normalizeLateUpsertItem(nextTurn.items, event.item)
      : event.item;
    const itemMerge = mergeCanonicalTurnItem(nextTurn.items, incomingItem);
    if (itemMerge.error) {
      return { state, changed: false, error: itemMerge.error };
    }
    nextTurn = {
      ...nextTurn,
      items: itemMerge.items,
    };
    changed = changed || itemMerge.changed;
  }

  if (existingIndex >= 0) {
    turns[existingIndex] = nextTurn;
  } else {
    turns.push(nextTurn);
  }
  turns = turns.sort(compareCanonicalTurns);

  const nextLastAppliedEventSeq = event.eventSeq > 0
    ? Math.max(state.lastAppliedEventSeq, event.eventSeq)
    : state.lastAppliedEventSeq;

  if (!changed && nextLastAppliedEventSeq === state.lastAppliedEventSeq) {
    return { state, changed: false };
  }

  return {
    state: {
      sessionId: normalizedSessionId,
      turns,
      lastAppliedEventSeq: nextLastAppliedEventSeq,
    },
    changed: true,
  };
}
