import { resolveMessageSemanticStage } from './timeline-semantic-stage';

export interface TimelineSemanticOrderInput {
  timestamp: number;
  stableId: string;
  messageType?: string;
  primaryToolCallName?: string;
  displayOrder?: number | null;
  itemOrder?: number | null;
  anchorEventSeq?: number;
  blockSeq?: number;
  cardStreamSeq?: number;
}

function normalizeNonNegativeInteger(value: number | null | undefined): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return null;
  }
  const normalized = Math.floor(value);
  return normalized >= 0 ? normalized : null;
}

function normalizePositiveInteger(value: number | null | undefined): number | null {
  const normalized = normalizeNonNegativeInteger(value);
  return normalized !== null && normalized > 0 ? normalized : null;
}

export function resolveTimelineAnchorTimestampFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number | null {
  const direct = metadata?.timelineAnchorTimestamp;
  if (typeof direct === 'number' && Number.isFinite(direct) && direct > 0) {
    return Math.floor(direct);
  }
  return null;
}

export function resolveTimelineEventSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  const raw = metadata?.eventSeq;
  if (typeof raw !== 'number' || !Number.isFinite(raw)) {
    return 0;
  }
  const normalized = Math.floor(raw);
  return normalized > 0 ? normalized : 0;
}

export function resolveTimelineBlockSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  const raw = metadata?.blockSeq;
  if (typeof raw !== 'number' || !Number.isFinite(raw)) {
    return 0;
  }
  const normalized = Math.floor(raw);
  return normalized >= 0 ? normalized : 0;
}

export function resolveTimelineCardStreamSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  const raw = metadata?.cardStreamSeq;
  if (typeof raw !== 'number' || !Number.isFinite(raw)) {
    return 0;
  }
  const normalized = Math.floor(raw);
  return normalized > 0 ? normalized : 0;
}

export function resolveTimelineSortTimestamp(
  timestamp: number | undefined,
  metadata: Record<string, unknown> | undefined,
): number {
  const anchorTimestamp = resolveTimelineAnchorTimestampFromMetadata(metadata);
  if (anchorTimestamp !== null) {
    return anchorTimestamp;
  }
  return typeof timestamp === 'number' && Number.isFinite(timestamp)
    ? Math.floor(timestamp)
    : 0;
}

/**
 * 时间轴节点一旦首次落位，就必须冻结其排序时间锚点。
 *
 * 这里不再做“取更早时间”的回溯修正，因为那会导致节点在 live 过程中
 * 因后续消息到达而发生重新排序，破坏“首次落位即定点”的产品约束。
 *
 * 规则：
 * - 已有有效时间锚点：直接保留
 * - 尚未建立有效时间锚点：采用本次 incoming 时间
 */
export function resolveStableTimelinePlacementTimestamp(
  currentTimestamp: number | undefined,
  incomingTimestamp: number | undefined,
): number {
  const existing = normalizePositiveInteger(currentTimestamp) || 0;
  if (existing > 0) {
    return existing;
  }
  return normalizePositiveInteger(incomingTimestamp) || 0;
}

export function resolveTimelineVersionFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  const eventSeq = resolveTimelineEventSeqFromMetadata(metadata);
  const cardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(metadata);
  return (eventSeq * 1_000_000) + cardStreamSeq;
}

export function resolveTimelineDetailedVersionFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  const eventSeq = resolveTimelineEventSeqFromMetadata(metadata);
  const blockSeq = resolveTimelineBlockSeqFromMetadata(metadata);
  const cardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(metadata);
  return (eventSeq * 1_000_000_000) + (blockSeq * 1_000_000) + cardStreamSeq;
}

export function compareTimelineSemanticOrder(
  left: TimelineSemanticOrderInput,
  right: TimelineSemanticOrderInput,
): number {
  const leftDisplayOrder = normalizeNonNegativeInteger(left.displayOrder);
  const rightDisplayOrder = normalizeNonNegativeInteger(right.displayOrder);
  if (leftDisplayOrder !== null && rightDisplayOrder !== null && leftDisplayOrder !== rightDisplayOrder) {
    return leftDisplayOrder - rightDisplayOrder;
  }

  const leftItemOrder = normalizePositiveInteger(left.itemOrder);
  const rightItemOrder = normalizePositiveInteger(right.itemOrder);
  if (leftItemOrder !== null && rightItemOrder !== null && leftItemOrder !== rightItemOrder) {
    return leftItemOrder - rightItemOrder;
  }

  if (left.timestamp !== right.timestamp) {
    return left.timestamp - right.timestamp;
  }

  const leftAnchorEventSeq = normalizePositiveInteger(left.anchorEventSeq);
  const rightAnchorEventSeq = normalizePositiveInteger(right.anchorEventSeq);
  if (
    leftAnchorEventSeq !== null
    && rightAnchorEventSeq !== null
    && leftAnchorEventSeq !== rightAnchorEventSeq
  ) {
    return leftAnchorEventSeq - rightAnchorEventSeq;
  }

  const leftBlockSeq = normalizeNonNegativeInteger(left.blockSeq) || 0;
  const rightBlockSeq = normalizeNonNegativeInteger(right.blockSeq) || 0;
  if (leftBlockSeq !== rightBlockSeq) {
    return leftBlockSeq - rightBlockSeq;
  }

  const leftStage = resolveMessageSemanticStage(left.messageType, left.primaryToolCallName || '');
  const rightStage = resolveMessageSemanticStage(right.messageType, right.primaryToolCallName || '');
  if (leftStage !== rightStage) {
    return leftStage - rightStage;
  }

  const leftCardStreamSeq = normalizeNonNegativeInteger(left.cardStreamSeq) || 0;
  const rightCardStreamSeq = normalizeNonNegativeInteger(right.cardStreamSeq) || 0;
  if (leftCardStreamSeq !== rightCardStreamSeq) {
    return leftCardStreamSeq - rightCardStreamSeq;
  }

  return left.stableId.localeCompare(right.stableId);
}
