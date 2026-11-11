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
