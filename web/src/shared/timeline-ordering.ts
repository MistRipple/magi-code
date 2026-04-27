export interface TimelineSemanticOrderInput {
  timestamp: number;
  stableId: string;
  turnSeq?: number;
  itemSeq?: number;
  laneSeq?: number;
  anchorEventSeq?: number;
  blockSeq?: number;
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

function resolveNonNegativeMetadataNumber(
  metadata: Record<string, unknown> | undefined,
  key: string,
): number {
  const raw = metadata?.[key];
  if (typeof raw !== 'number' || !Number.isFinite(raw)) {
    return 0;
  }
  const normalized = Math.floor(raw);
  return normalized >= 0 ? normalized : 0;
}

export function resolveTimelineTurnSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveNonNegativeMetadataNumber(metadata, 'turnSeq');
}

export function resolveTimelineItemSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveNonNegativeMetadataNumber(metadata, 'itemSeq');
}

export function resolveTimelineLaneSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveNonNegativeMetadataNumber(metadata, 'laneSeq');
}

export function resolveTimelineBlockSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveNonNegativeMetadataNumber(metadata, 'blockSeq');
}

export function resolveTimelineSemanticMessageType(
  messageType: string | undefined,
  metadata: Record<string, unknown> | undefined,
): string | undefined {
  const originMessageType = typeof metadata?.originMessageType === 'string'
    ? metadata.originMessageType.trim()
    : '';
  return originMessageType || messageType;
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
  return resolveTimelineEventSeqFromMetadata(metadata);
}

export function resolveTimelineDetailedVersionFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  const eventSeq = resolveTimelineEventSeqFromMetadata(metadata);
  const blockSeq = resolveTimelineBlockSeqFromMetadata(metadata);
  return (eventSeq * 1_000_000) + blockSeq;
}

function compareSharedFactOrder(
  left: number | null,
  right: number | null,
): number | null {
  if (left !== null && right !== null && left !== right) {
    return left - right;
  }
  return null;
}

export function compareTimelineSemanticOrder(
  left: TimelineSemanticOrderInput,
  right: TimelineSemanticOrderInput,
): number {
  const turnOrder = compareSharedFactOrder(
    normalizePositiveInteger(left.turnSeq),
    normalizePositiveInteger(right.turnSeq),
  );
  if (turnOrder !== null) {
    return turnOrder;
  }

  const itemOrder = compareSharedFactOrder(
    normalizePositiveInteger(left.itemSeq),
    normalizePositiveInteger(right.itemSeq),
  );
  if (itemOrder !== null) {
    return itemOrder;
  }

  const laneOrder = compareSharedFactOrder(
    normalizePositiveInteger(left.laneSeq),
    normalizePositiveInteger(right.laneSeq),
  );
  if (laneOrder !== null) {
    return laneOrder;
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

  if (leftAnchorEventSeq !== null && rightAnchorEventSeq !== null) {
    const leftBlockSeq = normalizeNonNegativeInteger(left.blockSeq) || 0;
    const rightBlockSeq = normalizeNonNegativeInteger(right.blockSeq) || 0;
    if (leftBlockSeq !== rightBlockSeq) {
      return leftBlockSeq - rightBlockSeq;
    }
  }

  return left.stableId.localeCompare(right.stableId);
}
