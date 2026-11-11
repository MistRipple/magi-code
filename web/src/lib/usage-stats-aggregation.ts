import type { PromptCacheStats } from '../web/agent-api';

export function aggregatePromptCacheStats(items: { cache?: PromptCacheStats }[]): PromptCacheStats {
  const sum = (field: 'observedCallCount' | 'hitCallCount' | 'inputTokens' | 'readTokens' | 'writeTokens') =>
    items.reduce((total, item) => total + (item.cache?.[field] ?? 0), 0);
  const observedCallCount = sum('observedCallCount');
  const hitCallCount = sum('hitCallCount');
  const inputTokens = sum('inputTokens');
  const readTokens = sum('readTokens');
  return {
    observedCallCount, hitCallCount, inputTokens, readTokens,
    writeTokens: sum('writeTokens'),
    tokenHitRate: inputTokens > 0 ? readTokens / inputTokens : null,
    requestHitRate: observedCallCount > 0 ? hitCallCount / observedCallCount : null,
  };
}

export interface UsageStatsItemLike {
  pluginId?: string | null;
  templateId: string;
  engineId: string;
  role: 'worker' | 'orchestrator' | 'auxiliary' | 'image_generation' | 'vision';
  llmCallCount: number;
  tokenObservedCallCount?: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
  resolvedModel?: string;
}

export function usageStatsDisplayKey(item: Pick<UsageStatsItemLike, 'pluginId' | 'role' | 'templateId'>): string {
  if (item.pluginId) return `plugin:${item.pluginId}`;
  if (item.role === 'image_generation') return 'imageGeneration';
  return item.role === 'worker' ? item.templateId.trim() : item.role;
}

export interface AggregatedUsageStats {
  totalExecutions: number;
  tokenObservedCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  successRate: number;
  totalInputTokens: number;
  totalOutputTokens: number;
  totalTokens: number;
  resolvedModel?: string;
  resolvedModels: string[];
}

export function aggregateUsageStatsForDisplay(
  items: UsageStatsItemLike[],
  targetKey: string,
): AggregatedUsageStats | null {
  const normalizedKey = targetKey.trim();
  const matched = items.filter((item) => usageStatsDisplayKey(item) === normalizedKey);

  if (matched.length === 0) {
    return null;
  }

  const totalExecutions = matched.reduce((sum, item) => sum + item.llmCallCount, 0);
  const assignmentCount = matched.reduce((sum, item) => sum + item.assignmentCount, 0);
  const successCount = matched.reduce((sum, item) => sum + item.successCount, 0);
  const failureCount = matched.reduce((sum, item) => sum + item.failureCount, 0);
  const totalInputTokens = matched.reduce((sum, item) => sum + item.netInputTokens, 0);
  const totalOutputTokens = matched.reduce((sum, item) => sum + item.netOutputTokens, 0);
  const totalTokens = totalInputTokens + totalOutputTokens;
  const resolvedModelsByKey = new Map<string, string>();
  for (const item of [...matched].sort((left, right) => (
    right.totalTokens - left.totalTokens
    || (left.resolvedModel || '').localeCompare(right.resolvedModel || '')
  ))) {
    const model = item.resolvedModel?.trim();
    if (!model) continue;
    const key = model.toLocaleLowerCase();
    if (!resolvedModelsByKey.has(key)) {
      resolvedModelsByKey.set(key, model);
    }
  }
  const resolvedModels = Array.from(resolvedModelsByKey.values());

  return {
    totalExecutions,
    tokenObservedCallCount: matched.reduce((sum, item) => sum + (item.tokenObservedCallCount ?? 0), 0),
    assignmentCount,
    successCount,
    failureCount,
    successRate: totalExecutions > 0 ? successCount / totalExecutions : 1,
    totalInputTokens,
    totalOutputTokens,
    totalTokens,
    resolvedModel: resolvedModels.length === 1 ? resolvedModels[0] : undefined,
    resolvedModels,
  };
}
