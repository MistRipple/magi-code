export interface UsageStatsItemLike {
  templateId: string;
  engineId: string;
  role: 'worker' | 'orchestrator' | 'auxiliary' | 'image_generation';
  llmCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
  resolvedModel?: string;
}

export interface AggregatedUsageStats {
  totalExecutions: number;
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
  const matched = items.filter((item) => {
    if (normalizedKey === 'orchestrator') {
      return item.role === 'orchestrator';
    }
    if (normalizedKey === 'auxiliary') {
      return item.role === 'auxiliary';
    }
    if (normalizedKey === 'imageGeneration') {
      return item.role === 'image_generation';
    }
    return item.role === 'worker' && item.templateId === normalizedKey;
  });

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
