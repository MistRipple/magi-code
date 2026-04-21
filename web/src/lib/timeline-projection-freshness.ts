import type { SessionTimelineProjection } from '../types/message';

type ProjectionLike = Pick<SessionTimelineProjection, 'lastAppliedEventSeq' | 'updatedAt' | 'artifacts'> | null | undefined;

interface ProjectionRichnessStats {
  artifactCount: number;
  executionItemCount: number;
  messageIdCount: number;
  blockCount: number;
  nonEmptyMessageCount: number;
  textLength: number;
  versionWeight: number;
  updatedAt: number;
}

function normalizeTimestamp(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.max(0, Math.floor(value)) : 0;
}

function collectMessageTextLength(message: Record<string, unknown> | undefined): number {
  if (!message || typeof message !== 'object') {
    return 0;
  }
  let total = typeof message.content === 'string' ? message.content.trim().length : 0;
  const blocks = Array.isArray(message.blocks) ? message.blocks : [];
  for (const block of blocks) {
    if (!block || typeof block !== 'object') {
      continue;
    }
    const record = block as Record<string, unknown>;
    const content = typeof record.content === 'string' ? record.content.trim().length : 0;
    const input = typeof record.input === 'string' ? record.input.trim().length : 0;
    const output = typeof record.output === 'string' ? record.output.trim().length : 0;
    const error = typeof record.error === 'string' ? record.error.trim().length : 0;
    const diff = typeof record.diff === 'string' ? record.diff.trim().length : 0;
    total += content + input + output + error + diff;
  }
  return total;
}

function collectProjectionRichnessStats(projection: ProjectionLike): ProjectionRichnessStats {
  const artifacts = Array.isArray(projection?.artifacts) ? projection!.artifacts : [];
  const stats: ProjectionRichnessStats = {
    artifactCount: artifacts.length,
    executionItemCount: 0,
    messageIdCount: 0,
    blockCount: 0,
    nonEmptyMessageCount: 0,
    textLength: 0,
    versionWeight: 0,
    updatedAt: normalizeTimestamp(projection?.updatedAt),
  };

  for (const artifact of artifacts) {
    const message = artifact.message as unknown as Record<string, unknown> | undefined;
    const executionItems = Array.isArray(artifact.executionItems)
      ? artifact.executionItems as unknown as Array<Record<string, unknown>>
      : [];

    stats.messageIdCount += Array.isArray(artifact.messageIds) ? artifact.messageIds.length : 0;
    stats.versionWeight += normalizeTimestamp(artifact.latestEventSeq) + normalizeTimestamp(artifact.cardStreamSeq);

    const artifactBlocks = Array.isArray(message?.blocks) ? message!.blocks.length : 0;
    const artifactTextLength = collectMessageTextLength(message);
    stats.blockCount += artifactBlocks;
    stats.textLength += artifactTextLength;
    if (artifactBlocks > 0 || artifactTextLength > 0) {
      stats.nonEmptyMessageCount += 1;
    }

    for (const item of executionItems) {
      const itemMessage = item.message as Record<string, unknown> | undefined;
      const itemBlocks = Array.isArray(itemMessage?.blocks) ? itemMessage!.blocks.length : 0;
      const itemTextLength = collectMessageTextLength(itemMessage);
      stats.executionItemCount += 1;
      stats.messageIdCount += Array.isArray(item.messageIds) ? item.messageIds.length : 0;
      stats.blockCount += itemBlocks;
      stats.textLength += itemTextLength;
      stats.versionWeight += normalizeTimestamp(item.latestEventSeq) + normalizeTimestamp(item.cardStreamSeq);
      if (itemBlocks > 0 || itemTextLength > 0) {
        stats.nonEmptyMessageCount += 1;
      }
    }
  }

  return stats;
}

function compareNumbers(left: number, right: number): number {
  return left === right ? 0 : left > right ? 1 : -1;
}

function compareProjectionRichnessStats(
  left: ProjectionRichnessStats,
  right: ProjectionRichnessStats,
): number {
  const comparisons: Array<[number, number]> = [
    [left.artifactCount, right.artifactCount],
    [left.executionItemCount, right.executionItemCount],
    [left.messageIdCount, right.messageIdCount],
    [left.blockCount, right.blockCount],
    [left.nonEmptyMessageCount, right.nonEmptyMessageCount],
    [left.textLength, right.textLength],
    [left.versionWeight, right.versionWeight],
  ];

  for (const [leftValue, rightValue] of comparisons) {
    if (leftValue !== rightValue) {
      return compareNumbers(leftValue, rightValue);
    }
  }

  return 0;
}

export function compareTimelineProjectionRichness(left: ProjectionLike, right: ProjectionLike): number {
  return compareProjectionRichnessStats(
    collectProjectionRichnessStats(left),
    collectProjectionRichnessStats(right),
  );
}

export function compareTimelineProjectionFreshness(left: ProjectionLike, right: ProjectionLike): number {
  const leftEventSeq = normalizeTimestamp(left?.lastAppliedEventSeq);
  const rightEventSeq = normalizeTimestamp(right?.lastAppliedEventSeq);
  if (leftEventSeq !== rightEventSeq) {
    return compareNumbers(leftEventSeq, rightEventSeq);
  }

  const leftStats = collectProjectionRichnessStats(left);
  const rightStats = collectProjectionRichnessStats(right);

  const comparisons: Array<[number, number]> = [
    [leftStats.artifactCount, rightStats.artifactCount],
    [leftStats.executionItemCount, rightStats.executionItemCount],
    [leftStats.messageIdCount, rightStats.messageIdCount],
    [leftStats.blockCount, rightStats.blockCount],
    [leftStats.nonEmptyMessageCount, rightStats.nonEmptyMessageCount],
    [leftStats.textLength, rightStats.textLength],
    [leftStats.versionWeight, rightStats.versionWeight],
    [leftStats.updatedAt, rightStats.updatedAt],
  ];

  for (const [leftValue, rightValue] of comparisons) {
    if (leftValue !== rightValue) {
      return compareNumbers(leftValue, rightValue);
    }
  }

  return 0;
}

export function shouldPreferRicherAuthoritativeProjection(
  incoming: ProjectionLike,
  current: ProjectionLike,
): boolean {
  const incomingStats = collectProjectionRichnessStats(incoming);
  const currentStats = collectProjectionRichnessStats(current);
  if (compareProjectionRichnessStats(incomingStats, currentStats) <= 0) {
    return false;
  }

  return incomingStats.updatedAt >= currentStats.updatedAt;
}
