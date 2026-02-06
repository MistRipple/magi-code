/**
 * 执行统计模块
 * 记录每个模型的执行历史、成功率、平均耗时等统计数据
 * 用于智能模型选择与替代建议决策
 */

import { logger, LogCategory } from '../logging';
import * as vscode from 'vscode';
import { globalEventBus } from '../events';
import { WorkerSlot } from '../types';

/** 执行阶段类型 */
export type ExecutionPhase = 'planning' | 'execution' | 'verification' | 'integration';

/** 单次执行记录 */
export interface ExecutionRecord {
  worker: string;
  taskId: string;
  subTaskId: string;
  success: boolean;
  duration: number;  // 毫秒
  error?: string;
  timestamp: number;
  /** Token 使用统计 */
  inputTokens?: number;
  outputTokens?: number;
  /** 执行阶段 */
  phase?: ExecutionPhase;
}

/** Worker 统计摘要 */
export interface WorkerStats {
  worker: string;
  totalExecutions: number;
  successCount: number;
  failureCount: number;
  successRate: number;  // 0-1
  avgDuration: number;  // 毫秒
  recentFailures: number;  // 最近10次中的失败次数
  lastFailureTime?: number;
  commonErrors: Map<string, number>;  // 错误类型 -> 出现次数
  isHealthy: boolean;  // 健康状态
  healthScore: number;  // 0-1
  /** 最近的错误信息 */
  lastError?: string;
  /** 最后执行时间 */
  lastExecutionTime?: number;
  /** 总输入 token */
  totalInputTokens: number;
  /** 总输出 token */
  totalOutputTokens: number;
}


/** 替代建议 */
export interface AlternativeSuggestion {
  originalWorker: WorkerSlot;
  suggestedWorker: WorkerSlot;
  reason: string;
  confidence: number;  // 0-1
}

/** 统计配置 */
export interface StatsConfig {
  maxRecords: number;  // 最大记录数
  recentWindow: number;  // 最近窗口大小
  healthThreshold: number;  // 健康阈值（成功率）
  persistKey: string;  // 持久化键名
}

const DEFAULT_CONFIG: StatsConfig = {
  maxRecords: 1000,
  recentWindow: 10,
  healthThreshold: 0.7,
  persistKey: 'multiCli.executionStats',
};

/**
 * 执行统计管理器
 */
export class ExecutionStats {
  private records: ExecutionRecord[] = [];
  private config: StatsConfig;
  private context?: vscode.ExtensionContext;

  constructor(config?: Partial<StatsConfig>) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /** 更新统计配置 */
  configure(config: Partial<StatsConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /** 设置扩展上下文（用于持久化） */
  setContext(context: vscode.ExtensionContext): void {
    this.context = context;
    this.loadFromStorage();
  }

  /** 记录一次执行 */
  recordExecution(record: Omit<ExecutionRecord, 'timestamp'>): void {
    const fullRecord: ExecutionRecord = {
      ...record,
      timestamp: Date.now(),
    };

    this.records.push(fullRecord);

    // 限制记录数量
    if (this.records.length > this.config.maxRecords) {
      this.records = this.records.slice(-this.config.maxRecords);
    }

    // 异步保存
    this.saveToStorage();

    logger.info(
      '编排器.执行.已记录',
      { worker: record.worker, success: record.success, duration: record.duration, taskId: record.taskId, subTaskId: record.subTaskId },
      LogCategory.ORCHESTRATOR
    );
    globalEventBus.emitEvent('execution:stats_updated', {});
  }

  /** 获取 Worker 统计摘要 */
  getStats(worker: string): WorkerStats {
    const workerRecords = this.records.filter(r => r.worker === worker);
    const recentRecords = workerRecords.slice(-this.config.recentWindow);

    const totalExecutions = workerRecords.length;
    const successCount = workerRecords.filter(r => r.success).length;
    const failureCount = totalExecutions - successCount;
    const successRate = totalExecutions > 0 ? successCount / totalExecutions : 1;

    const successfulDurations = workerRecords.filter(r => r.success).map(r => r.duration);
    const avgDuration = successfulDurations.length > 0
      ? successfulDurations.reduce((a, b) => a + b, 0) / successfulDurations.length
      : 0;

    const recentFailures = recentRecords.filter(r => !r.success).length;
    const lastFailure = workerRecords.filter(r => !r.success).pop();
    const lastRecord = workerRecords[workerRecords.length - 1];

    // 统计常见错误
    const commonErrors = new Map<string, number>();
    workerRecords.filter(r => r.error).forEach(r => {
      const errorType = this.categorizeError(r.error!);
      commonErrors.set(errorType, (commonErrors.get(errorType) || 0) + 1);
    });


    const totalInputTokens = workerRecords.reduce((sum, r) => sum + (r.inputTokens || 0), 0);
    const totalOutputTokens = workerRecords.reduce((sum, r) => sum + (r.outputTokens || 0), 0);

    const recentFailureRate = recentRecords.length > 0 ? recentFailures / recentRecords.length : 0;
    const healthScore = Math.max(0, Math.min(1, successRate - recentFailureRate * 0.5));

    // 判断健康状态
    const isHealthy = healthScore >= this.config.healthThreshold;

    return {
      worker,
      totalExecutions,
      successCount,
      failureCount,
      successRate,
      avgDuration,
      recentFailures,
      lastFailureTime: lastFailure?.timestamp,
      commonErrors,
      isHealthy,
      healthScore,
      lastError: lastFailure?.error,
      lastExecutionTime: lastRecord?.timestamp,
      totalInputTokens,
      totalOutputTokens,
    };
  }

  /** 获取所有 Worker 的统计 */
  getAllStats(modelIds?: string[]): WorkerStats[] {
    const defaultWorkers: WorkerSlot[] = ['claude', 'codex', 'gemini'];
    const ids = modelIds && modelIds.length > 0 ? new Set(modelIds) : new Set(defaultWorkers);

    if (modelIds && modelIds.length > 0) {
      for (const record of this.records) {
        if (record.worker) {
          ids.add(record.worker);
        }
      }
    }

    return Array.from(ids).map(id => this.getStats(id));
  }

  /** 获取按阶段分离的 Token 统计 */
  getPhaseStats(): Record<ExecutionPhase | 'unknown', { inputTokens: number; outputTokens: number }> {
    const phases: (ExecutionPhase | 'unknown')[] = ['planning', 'execution', 'verification', 'integration', 'unknown'];
    const result: Record<string, { inputTokens: number; outputTokens: number }> = {};

    for (const phase of phases) {
      const phaseRecords = phase === 'unknown'
        ? this.records.filter(r => !r.phase)
        : this.records.filter(r => r.phase === phase);

      result[phase] = {
        inputTokens: phaseRecords.reduce((sum, r) => sum + (r.inputTokens || 0), 0),
        outputTokens: phaseRecords.reduce((sum, r) => sum + (r.outputTokens || 0), 0),
      };
    }

    return result as Record<ExecutionPhase | 'unknown', { inputTokens: number; outputTokens: number }>;
  }

  /** 获取总 Token 统计 */
  getTotalTokens(): { inputTokens: number; outputTokens: number } {
    return {
      inputTokens: this.records.reduce((sum, r) => sum + (r.inputTokens || 0), 0),
      outputTokens: this.records.reduce((sum, r) => sum + (r.outputTokens || 0), 0),
    };
  }

  /** 获取健康的 Worker 列表 */
  getHealthyWorkers(): WorkerSlot[] {
    return this.getAllStats()
      .filter(s => s.isHealthy && this.isWorkerSlot(s.worker))
      .map(s => s.worker as WorkerSlot);
  }

  /** 获取替代建议 */
  getAlternativeSuggestion(
    failedWorker: WorkerSlot,
    excludeWorkers: WorkerSlot[] = [],
    availableWorkers?: WorkerSlot[]
  ): AlternativeSuggestion | null {
    const allStats = this.getAllStats();
    const candidates = allStats
      .filter(s => this.isWorkerSlot(s.worker))
      .filter(s => s.worker !== failedWorker && !excludeWorkers.includes(s.worker as WorkerSlot))
      .filter(s => !availableWorkers || availableWorkers.includes(s.worker as WorkerSlot))
      .filter(s => s.isHealthy || s.totalExecutions < 3)  // 健康或样本不足
      .sort((a, b) => {
        // 优先选择成功率高的
        if (Math.abs(a.successRate - b.successRate) > 0.1) {
          return b.successRate - a.successRate;
        }
        // 成功率相近时，选择平均耗时短的
        return a.avgDuration - b.avgDuration;
      });

    if (candidates.length === 0) {
      return null;
    }

    const best = candidates[0];
    const failedStats = this.getStats(failedWorker);

    return {
      originalWorker: failedWorker,
      suggestedWorker: best.worker as WorkerSlot,
      reason: this.buildSuggestionReason(failedStats, best),
      confidence: this.calculateConfidence(best),
    };
  }

  /** 根据任务类型推荐最佳 Worker */
  recommendWorker(taskCategory: string, availableWorkers: WorkerSlot[]): WorkerSlot {
    const stats = availableWorkers.map(worker => this.getStats(worker));

    // 过滤掉不健康的 Worker（除非全部不健康）
    const healthyStats = stats.filter(s => s.isHealthy);
    const candidates = healthyStats.length > 0 ? healthyStats : stats;

    // 按成功率和平均耗时排序
    candidates.sort((a, b) => {
      const scoreA = a.successRate * 100 - a.avgDuration / 1000;
      const scoreB = b.successRate * 100 - b.avgDuration / 1000;
      return scoreB - scoreA;
    });

    return (candidates[0]?.worker as WorkerSlot) || availableWorkers[0];
  }

  private isWorkerSlot(value: string): value is WorkerSlot {
    return value === 'claude' || value === 'codex' || value === 'gemini';
  }

  /** 分类错误类型 */
  private categorizeError(error: string): string {
    const lowerError = error.toLowerCase();
    if (lowerError.includes('timeout') || lowerError.includes('超时')) return 'timeout';
    if (lowerError.includes('rate limit') || lowerError.includes('限流')) return 'rate_limit';
    if (lowerError.includes('auth') || lowerError.includes('认证')) return 'auth';
    if (lowerError.includes('network') || lowerError.includes('网络')) return 'network';
    if (lowerError.includes('quota') || lowerError.includes('配额')) return 'quota';
    return 'unknown';
  }

  /** 构建建议原因说明 */
  private buildSuggestionReason(failed: WorkerStats, suggested: WorkerStats): string {
    const reasons: string[] = [];

    if (failed.recentFailures > 3) {
      reasons.push(`${failed.worker} 最近失败率较高 (${failed.recentFailures}/${this.config.recentWindow})`);
    }
    if (suggested.successRate > failed.successRate) {
      reasons.push(`${suggested.worker} 成功率更高 (${(suggested.successRate * 100).toFixed(0)}%)`);
    }
    if (suggested.avgDuration < failed.avgDuration && failed.avgDuration > 0) {
      reasons.push(`${suggested.worker} 平均响应更快`);
    }

    return reasons.join('；') || `${suggested.worker} 当前状态更稳定`;
  }

  /** 计算建议的置信度 */
  private calculateConfidence(stats: WorkerStats): number {
    const sampleFactor = Math.min(stats.totalExecutions / 10, 1);
    const successFactor = stats.successRate;
    const recentFactor = 1 - (stats.recentFailures / this.config.recentWindow);
    return (sampleFactor * 0.3 + successFactor * 0.4 + recentFactor * 0.3);
  }

  /** 从存储加载数据 */
  private loadFromStorage(): void {
    if (!this.context) return;

    try {
      const raw = this.context.globalState.get<any>(this.config.persistKey);
      if (raw && typeof raw === 'object' && Array.isArray(raw.records)) {
        this.records = raw.records;
        logger.info('编排器.执行_统计.加载.完成', { count: this.records.length }, LogCategory.ORCHESTRATOR);
      }
    } catch (error) {
      logger.warn('编排器.执行_统计.加载.失败', error, LogCategory.ORCHESTRATOR);
    }
  }

  /** 保存数据到存储 */
  private async saveToStorage(): Promise<void> {
    if (!this.context) return;

    try {
      await this.context.globalState.update(this.config.persistKey, {
        version: 1,
        records: this.records,
      });
    } catch (error) {
      logger.warn('编排器.执行_统计.保存.失败', error, LogCategory.ORCHESTRATOR);
    }
  }

  /** 清除所有统计数据 */
  async clearStats(): Promise<void> {
    this.records = [];
    await this.saveToStorage();
    logger.info('编排器.执行_统计.已清理', undefined, LogCategory.ORCHESTRATOR);
    globalEventBus.emitEvent('execution:stats_updated', {});
  }

  /** 获取统计摘要（用于 UI 显示） */
  getSummary(): string {
    const allStats = this.getAllStats();
    return allStats.map(s =>
      `${s.worker}: ${(s.successRate * 100).toFixed(0)}% (${s.totalExecutions}次)`
    ).join(' | ');
  }
}
