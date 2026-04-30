export interface TimelineSemanticOrderInput {
  turnOrderSeq: number;
  itemSeq: number;
  laneSeq?: number;
  blockSeq?: number;
  displayOrder: number;
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

export function resolveTimelineCanonicalTurnSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveNonNegativeMetadataNumber(metadata, 'turnSeq');
}

export function resolveTimelineTurnOrderSeqFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveNonNegativeMetadataNumber(metadata, 'turnOrderSeq')
    || resolveTimelineCanonicalTurnSeqFromMetadata(metadata);
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

export function resolveTimelineSemanticMessageType(
  messageType: string | undefined,
  metadata: Record<string, unknown> | undefined,
): string | undefined {
  const originMessageType = typeof metadata?.originMessageType === 'string'
    ? metadata.originMessageType.trim()
    : '';
  return originMessageType || messageType;
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

export function resolveTimelineSortTimestamp(
  timestamp: number | undefined,
  _metadata?: Record<string, unknown> | undefined,
): number {
  return typeof timestamp === 'number' && Number.isFinite(timestamp)
    ? Math.floor(timestamp)
    : 0;
}

export function resolveTimelineVersionFromMetadata(
  metadata: Record<string, unknown> | undefined,
): number {
  return resolveTimelineEventSeqFromMetadata(metadata);
}

function normalizeTimelineSequence(value: number | null | undefined): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return null;
  }
  const normalized = Math.floor(value);
  return normalized > 0 ? normalized : null;
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
    normalizeTimelineSequence(left.turnOrderSeq),
    normalizeTimelineSequence(right.turnOrderSeq),
  );
  if (turnOrder !== null) {
    return turnOrder;
  }

  const itemOrder = compareSharedFactOrder(
    normalizeTimelineSequence(left.itemSeq),
    normalizeTimelineSequence(right.itemSeq),
  );
  if (itemOrder !== null) {
    return itemOrder;
  }

  const laneOrder = compareSharedFactOrder(
    normalizeTimelineSequence(left.laneSeq),
    normalizeTimelineSequence(right.laneSeq),
  );
  if (laneOrder !== null) {
    return laneOrder;
  }

  const blockOrder = compareSharedFactOrder(
    normalizeTimelineSequence(left.blockSeq),
    normalizeTimelineSequence(right.blockSeq),
  );
  if (blockOrder !== null) {
    return blockOrder;
  }

  // displayOrder 作为最终稳定本地落位序号，永不由时间戳重算。
  if (left.displayOrder !== right.displayOrder) {
    return left.displayOrder - right.displayOrder;
  }

  return 0;
}
