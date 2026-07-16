export type TurnNavigationStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'blocked'
  | 'failed'
  | 'cancelled';

export interface TurnNavigationMessage {
  id: string;
  turnId: string;
  turnSeq: number;
  turnStatus: TurnNavigationStatus;
  type: string;
  content: string;
}

export interface TurnNavigationItem {
  turnId: string;
  turnSeq: number;
  index: number;
  status: TurnNavigationStatus;
  messageIds: string[];
  anchorMessageId: string;
  summary: string;
}

export interface TurnNavigationRailLayout {
  itemSpacing: number;
  verticalPadding: number;
  markerHeight: number;
  viewportHeight: number;
  contentHeight: number;
  maxScrollTop: number;
  scrollable: boolean;
}

interface MutableTurnNavigationItem extends Omit<TurnNavigationItem, 'index'> {
  fallbackSummary: string;
}

const TURN_NAVIGATION_STATUSES = new Set<TurnNavigationStatus>([
  'pending',
  'running',
  'completed',
  'blocked',
  'failed',
  'cancelled',
]);

export function isTurnNavigationStatus(value: unknown): value is TurnNavigationStatus {
  return typeof value === 'string' && TURN_NAVIGATION_STATUSES.has(value as TurnNavigationStatus);
}

function normalizeSummary(value: string): string {
  return value.replace(/\s+/gu, ' ').trim();
}

export function buildTurnNavigationItems(
  messages: readonly TurnNavigationMessage[],
): TurnNavigationItem[] {
  const itemsByTurnId = new Map<string, MutableTurnNavigationItem>();

  for (const message of messages) {
    const existing = itemsByTurnId.get(message.turnId);
    const content = normalizeSummary(message.content);
    if (!existing) {
      itemsByTurnId.set(message.turnId, {
        turnId: message.turnId,
        turnSeq: message.turnSeq,
        status: message.turnStatus,
        messageIds: [message.id],
        anchorMessageId: message.id,
        summary: message.type === 'user_input' ? content : '',
        fallbackSummary: content,
      });
      continue;
    }

    existing.messageIds.push(message.id);
    existing.status = message.turnStatus;
    if (!existing.summary && message.type === 'user_input' && content) {
      existing.summary = content;
    }
    if (!existing.fallbackSummary && content) {
      existing.fallbackSummary = content;
    }
  }

  return [...itemsByTurnId.values()]
    .sort((left, right) => left.turnSeq - right.turnSeq)
    .map(({ fallbackSummary, ...item }, index) => ({
      ...item,
      index: index + 1,
      summary: item.summary || fallbackSummary,
    }));
}

export function calculateTurnNavigationMagnet(
  markerPositions: readonly number[],
  pointerY: number,
  influenceRadius = 145,
): { focusIndex: number; strengths: number[] } {
  let focusIndex = -1;
  let nearestDistance = Number.POSITIVE_INFINITY;
  const strengths = markerPositions.map((markerY, index) => {
    const distance = Math.abs(pointerY - markerY);
    if (distance < nearestDistance) {
      nearestDistance = distance;
      focusIndex = index;
    }
    const linearStrength = Math.max(0, 1 - distance / influenceRadius);
    return linearStrength * linearStrength * (3 - 2 * linearStrength);
  });
  return {
    focusIndex,
    strengths: strengths.map((strength, index) => (
      index === focusIndex ? 1 : Math.min(strength, 0.68)
    )),
  };
}

export function calculateTurnNavigationMarkerOffsets(
  itemCount: number,
  itemSpacing = 13,
  verticalPadding = 12,
  markerHeight = 2,
): number[] {
  const count = Math.max(0, Math.floor(itemCount));
  return Array.from({ length: count }, (_, index) => (
    verticalPadding + markerHeight / 2 + index * itemSpacing
  ));
}

export function calculateTurnNavigationRailLayout(
  itemCount: number,
  viewportHeight: number,
  itemSpacing = 13,
  verticalPadding = 12,
  markerHeight = 2,
): TurnNavigationRailLayout {
  const markerOffsets = calculateTurnNavigationMarkerOffsets(
    itemCount,
    itemSpacing,
    verticalPadding,
    markerHeight,
  );
  const contentHeight = markerOffsets.length === 0
    ? verticalPadding * 2
    : markerOffsets[markerOffsets.length - 1] + markerHeight / 2 + verticalPadding;
  const normalizedViewportHeight = Math.max(0, viewportHeight);
  const maxScrollTop = Math.max(0, contentHeight - normalizedViewportHeight);
  return {
    itemSpacing,
    verticalPadding,
    markerHeight,
    viewportHeight: normalizedViewportHeight,
    contentHeight,
    maxScrollTop,
    scrollable: maxScrollTop > 0,
  };
}

export function calculateTurnNavigationScrollTarget(
  markerTop: number,
  markerHeight: number,
  viewportHeight: number,
  currentScrollTop: number,
  contentHeight: number,
): number {
  const maxScrollTop = Math.max(0, contentHeight - Math.max(0, viewportHeight));
  const centeredTarget = markerTop + markerHeight / 2 - Math.max(0, viewportHeight) / 2;
  const target = Math.min(maxScrollTop, Math.max(0, centeredTarget));
  return Math.abs(target - currentScrollTop) < 1 ? currentScrollTop : target;
}

export function isTurnNavigationNeighbor(index: number, focusIndex: number): boolean {
  return focusIndex >= 0 && Math.abs(index - focusIndex) === 1;
}
