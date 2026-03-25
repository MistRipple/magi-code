import type { FullLLMConfig } from '../llm/types';
import type { ExecutionStats, WorkerStats } from '../orchestrator/execution-stats';

export interface ExecutionStatsCatalogEntry {
  id: string;
  label: string;
  model?: string;
  provider?: string;
  enabled?: boolean;
  role?: 'worker' | 'orchestrator' | 'auxiliary';
}

export interface ExecutionStatsItem {
  worker: string;
  provider: 'openai' | 'anthropic' | 'unknown';
  totalExecutions: number;
  successCount: number;
  failureCount: number;
  successRate: number;
  avgDuration: number;
  isHealthy: boolean;
  healthScore: number;
  lastError?: string;
  lastExecutionTime?: number;
  totalInputTokens: number;
  totalOutputTokens: number;
}

export interface ExecutionStatsSummary {
  totalTasks: number;
  totalSuccess: number;
  totalFailed: number;
  totalInputTokens: number;
  totalOutputTokens: number;
  totalTokens: number;
}

export interface ExecutionStatsPayload {
  stats: ExecutionStatsItem[];
  orchestratorStats: ExecutionStatsSummary;
  modelCatalog: ExecutionStatsCatalogEntry[];
}

type ExecutionStatsReader = Pick<ExecutionStats, 'getAllStats'>;

function normalizeProvider(provider?: string): 'openai' | 'anthropic' | 'unknown' {
  if (provider === 'openai' || provider === 'anthropic') {
    return provider;
  }
  return 'unknown';
}

function toLabel(id: string): string {
  return id.charAt(0).toUpperCase() + id.slice(1);
}

function buildEmptyWorkerStats(worker: string): WorkerStats {
  return {
    worker,
    totalExecutions: 0,
    successCount: 0,
    failureCount: 0,
    successRate: 1,
    avgDuration: 0,
    recentFailures: 0,
    commonErrors: new Map<string, number>(),
    isHealthy: true,
    healthScore: 1,
    totalInputTokens: 0,
    totalOutputTokens: 0,
  };
}

export function buildModelCatalogFromLLMConfig(
  fullConfig?: Partial<FullLLMConfig> | null,
): ExecutionStatsCatalogEntry[] {
  const entries: ExecutionStatsCatalogEntry[] = [];
  const addEntry = (
    id: string,
    config: unknown,
    role: 'worker' | 'orchestrator' | 'auxiliary',
    label?: string,
  ): void => {
    const normalizedConfig = config && typeof config === 'object'
      ? config as Record<string, unknown>
      : {};
    entries.push({
      id,
      label: label || toLabel(id),
      model: typeof normalizedConfig.model === 'string' ? normalizedConfig.model : undefined,
      provider: typeof normalizedConfig.provider === 'string' ? normalizedConfig.provider : undefined,
      enabled: normalizedConfig.enabled !== false,
      role,
    });
  };

  const workerConfigs = fullConfig?.workers && typeof fullConfig.workers === 'object'
    ? fullConfig.workers as unknown as Record<string, unknown>
    : {};
  for (const [workerId, workerConfig] of Object.entries(workerConfigs)) {
    addEntry(workerId, workerConfig, 'worker');
  }

  addEntry('orchestrator', fullConfig?.orchestrator, 'orchestrator', 'Orchestrator');
  addEntry('auxiliary', fullConfig?.auxiliary, 'auxiliary', 'Auxiliary');

  return entries;
}

export function buildExecutionStatsPayload(
  executionStats: ExecutionStatsReader | null | undefined,
  modelCatalog: ExecutionStatsCatalogEntry[],
): ExecutionStatsPayload {
  const catalogMap = new Map(modelCatalog.map((entry) => [entry.id, entry]));
  const modelIds = modelCatalog.map((entry) => entry.id);
  const workerStatsList = executionStats
    ? executionStats.getAllStats(modelIds)
    : modelIds.map((id) => buildEmptyWorkerStats(id));

  const stats = workerStatsList.map((workerStats) => ({
    worker: workerStats.worker,
    provider: normalizeProvider(catalogMap.get(workerStats.worker)?.provider),
    totalExecutions: workerStats.totalExecutions,
    successCount: workerStats.successCount,
    failureCount: workerStats.failureCount,
    successRate: workerStats.successRate,
    avgDuration: workerStats.avgDuration,
    isHealthy: workerStats.isHealthy,
    healthScore: workerStats.healthScore,
    lastError: workerStats.lastError,
    lastExecutionTime: workerStats.lastExecutionTime,
    totalInputTokens: workerStats.totalInputTokens,
    totalOutputTokens: workerStats.totalOutputTokens,
  }));

  return {
    stats,
    orchestratorStats: {
      totalTasks: stats.reduce((sum, item) => sum + item.totalExecutions, 0),
      totalSuccess: stats.reduce((sum, item) => sum + item.successCount, 0),
      totalFailed: stats.reduce((sum, item) => sum + item.failureCount, 0),
      totalInputTokens: stats.reduce((sum, item) => sum + item.totalInputTokens, 0),
      totalOutputTokens: stats.reduce((sum, item) => sum + item.totalOutputTokens, 0),
      totalTokens: stats.reduce((sum, item) => sum + item.totalInputTokens + item.totalOutputTokens, 0),
    },
    modelCatalog,
  };
}
