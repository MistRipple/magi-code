export interface UsageStatsItemLike {
  templateId: string;
  engineId: string;
  role: 'worker' | 'orchestrator' | 'auxiliary';
  llmCallCount: number;
  assignmentCount: number;
  successCount: number;
  failureCount: number;
  totalTokens: number;
  netInputTokens: number;
  netOutputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
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
    return item.role === 'worker' && (item.engineId === normalizedKey || item.templateId === normalizedKey);
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
  const totalTokens = matched.reduce((sum, item) => sum + item.totalTokens, 0);
  const resolvedModels = Array.from(new Set(matched.map((item) => item.resolvedModel).filter((value): value is string => typeof value === 'string' && value.trim().length > 0)));

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
  };
}

