import type { OrchestrationRuntimeTimelineEntry } from '../types/message';

export function compareRuntimeTimelineLatestFirst(
  left: OrchestrationRuntimeTimelineEntry,
  right: OrchestrationRuntimeTimelineEntry,
): number {
  return right.timestamp - left.timestamp
    || right.seq - left.seq
    || right.eventId.localeCompare(left.eventId);
}

export function mergeRuntimeTimelineEntries(
  ...groups: ReadonlyArray<readonly OrchestrationRuntimeTimelineEntry[]>
): OrchestrationRuntimeTimelineEntry[] {
  const seen = new Set<string>();
  return groups
    .flatMap((group) => group)
    .sort(compareRuntimeTimelineLatestFirst)
    .filter((entry) => {
      const key = runtimeTimelineDedupKey(entry);
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    })
    .slice(0, 8);
}

export function mergeCurrentRuntimeTimelineEntries(input: {
  runtimeEntries: readonly OrchestrationRuntimeTimelineEntry[];
  conversationEntries: readonly OrchestrationRuntimeTimelineEntry[];
  isProcessing: boolean;
  processingStartedAt?: number | null;
  currentTurnStartedAt?: number | null;
}): OrchestrationRuntimeTimelineEntry[] {
  const processingStartedAt = typeof input.processingStartedAt === 'number'
    && Number.isFinite(input.processingStartedAt)
    ? input.processingStartedAt
    : 0;
  const currentTurnStartedAt = typeof input.currentTurnStartedAt === 'number'
    && Number.isFinite(input.currentTurnStartedAt)
    ? input.currentTurnStartedAt
    : 0;
  const timelineStartedAt = currentTurnStartedAt > 0
    ? currentTurnStartedAt
    : input.isProcessing
      ? processingStartedAt
      : 0;
  const runtimeEntries = timelineStartedAt > 0
    ? input.runtimeEntries.filter((entry) => entry.timestamp >= timelineStartedAt)
    : input.runtimeEntries;
  const conversationKeys = new Set(input.conversationEntries.map(runtimeTimelineDedupKey));
  const conversationSingletonTypes = new Set<string>(
    input.conversationEntries
      .map((entry) => entry.type.trim().toLowerCase())
      .filter((type) => type === 'session.turn.interrupted'),
  );
  const productRuntimeEntries = input.conversationEntries.length > 0
    ? runtimeEntries
        .filter(runtimeEntryNeedsAttention)
        .filter((entry) => !conversationKeys.has(runtimeTimelineDedupKey(entry)))
        .filter((entry) => !conversationSingletonTypes.has(entry.type.trim().toLowerCase()))
    : runtimeEntries;
  return mergeRuntimeTimelineEntries(productRuntimeEntries, input.conversationEntries);
}

export function runtimeTimelineDedupKey(entry: OrchestrationRuntimeTimelineEntry): string {
  const detail = normalizeTimelineDetailKey(entry.detail);
  if (detail) {
    return `detail:${detail}`;
  }
  return [entry.kind || 'progress', entry.type, entry.summary]
    .map((value) => normalizeRuntimeText(value).toLowerCase().replace(/\s+/g, ' '))
    .join('|');
}

function normalizeTimelineDetailKey(value: string | undefined): string {
  return normalizeRuntimeText(value)
    .replace(/^[a-z][a-z0-9_.-]{2,80}:\s+/i, '')
    .toLowerCase()
    .replace(/\s+/g, ' ');
}

function normalizeRuntimeText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function runtimeEntryNeedsAttention(entry: OrchestrationRuntimeTimelineEntry): boolean {
  if (entry.kind === 'error' || entry.kind === 'warning') return true;
  const type = entry.type.trim().toLowerCase();
  return type.includes('failed')
    || type.includes('interrupted')
    || type.includes('blocked');
}
