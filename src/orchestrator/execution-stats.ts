/**
 * 执行统计模块
 * 记录每个 CLI 的执行历史、成功率、平均耗时等统计数据
 * 用于智能 CLI 选择和降级决策
 */

import * as vscode from 'vscode';
import { globalEventBus } from '../events';
import { CLIType } from '../types';

/** 单次执行记录 */
export interface ExecutionRecord {
  cli: CLIType;
  taskId: string;
  subTaskId: string;
  success: boolean;
  duration: number;  // 毫秒
  error?: string;
  timestamp: number;
  /** 🆕 Token 使用统计 */
  inputTokens?: number;
  outputTokens?: number;
}

/** CLI 统计摘要 */
export interface CLIStats {
  cli: CLIType;
  totalExecutions: number;
  successCount: number;
  failureCount: number;
  successRate: number;  // 0-1
  avgDuration: number;  // 毫秒
  recentFailures: number;  // 最近10次中的失败次数
  lastFailureTime?: number;
  commonErrors: Map<string, number>;  // 错误类型 -> 出现次数
  isHealthy: boolean;  // 健康状态
  /** 🆕 最近的错误信息 */
  lastError?: string;
  /** 🆕 最后执行时间 */
  lastExecutionTime?: number;
  /** 🆕 总输入 token */
  totalInputTokens: number;
  /** 🆕 总输出 token */
  totalOutputTokens: number;
}

/** 降级建议 */
export interface FallbackSuggestion {
  originalCli: CLIType;
  suggestedCli: CLIType;
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

    console.log(`[ExecutionStats] 记录执行: ${record.cli} ${record.success ? '成功' : '失败'} (${record.duration}ms)`);
    globalEventBus.emitEvent('execution:stats_updated', {});
  }

  /** 获取 CLI 统计摘要 */
  getStats(cli: CLIType): CLIStats {
    const cliRecords = this.records.filter(r => r.cli === cli);
    const recentRecords = cliRecords.slice(-this.config.recentWindow);

    const totalExecutions = cliRecords.length;
    const successCount = cliRecords.filter(r => r.success).length;
    const failureCount = totalExecutions - successCount;
    const successRate = totalExecutions > 0 ? successCount / totalExecutions : 1;

    const successfulDurations = cliRecords.filter(r => r.success).map(r => r.duration);
    const avgDuration = successfulDurations.length > 0
      ? successfulDurations.reduce((a, b) => a + b, 0) / successfulDurations.length
      : 0;

    const recentFailures = recentRecords.filter(r => !r.success).length;
    const lastFailure = cliRecords.filter(r => !r.success).pop();
    const lastRecord = cliRecords[cliRecords.length - 1];

    // 统计常见错误
    const commonErrors = new Map<string, number>();
    cliRecords.filter(r => r.error).forEach(r => {
      const errorType = this.categorizeError(r.error!);
      commonErrors.set(errorType, (commonErrors.get(errorType) || 0) + 1);
    });

    // 🆕 计算总 token
    const totalInputTokens = cliRecords.reduce((sum, r) => sum + (r.inputTokens || 0), 0);
    const totalOutputTokens = cliRecords.reduce((sum, r) => sum + (r.outputTokens || 0), 0);

    // 判断健康状态
    const isHealthy = successRate >= this.config.healthThreshold &&
      recentFailures < this.config.recentWindow * 0.5;

    return {
      cli,
      totalExecutions,
      successCount,
      failureCount,
      successRate,
      avgDuration,
      recentFailures,
      lastFailureTime: lastFailure?.timestamp,
      commonErrors,
      isHealthy,
      lastError: lastFailure?.error,
      lastExecutionTime: lastRecord?.timestamp,
      totalInputTokens,
      totalOutputTokens,
    };
  }

  /** 获取所有 CLI 的统计 */
  getAllStats(): CLIStats[] {
    const cliTypes: CLIType[] = ['claude', 'codex', 'gemini'];
    return cliTypes.map(cli => this.getStats(cli));
  }

  /** 获取健康的 CLI 列表 */
  getHealthyCLIs(): CLIType[] {
    return this.getAllStats()
      .filter(s => s.isHealthy)
      .map(s => s.cli);
  }

  /** 获取降级建议 */
  getFallbackSuggestion(failedCli: CLIType, excludeClis: CLIType[] = []): FallbackSuggestion | null {
    const allStats = this.getAllStats();
    const candidates = allStats
      .filter(s => s.cli !== failedCli && !excludeClis.includes(s.cli))
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
    const failedStats = this.getStats(failedCli);

    return {
      originalCli: failedCli,
      suggestedCli: best.cli,
      reason: this.buildFallbackReason(failedStats, best),
      confidence: this.calculateConfidence(best),
    };
  }

  /** 根据任务类型推荐最佳 CLI */
  recommendCLI(taskCategory: string, availableCLIs: CLIType[]): CLIType {
    const stats = availableCLIs.map(cli => this.getStats(cli));

    // 过滤掉不健康的 CLI（除非全部不健康）
    const healthyStats = stats.filter(s => s.isHealthy);
    const candidates = healthyStats.length > 0 ? healthyStats : stats;

    // 按成功率和平均耗时排序
    candidates.sort((a, b) => {
      const scoreA = a.successRate * 100 - a.avgDuration / 1000;
      const scoreB = b.successRate * 100 - b.avgDuration / 1000;
      return scoreB - scoreA;
    });

    return candidates[0]?.cli || availableCLIs[0];
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

  /** 构建降级原因说明 */
  private buildFallbackReason(failed: CLIStats, suggested: CLIStats): string {
    const reasons: string[] = [];

    if (failed.recentFailures > 3) {
      reasons.push(`${failed.cli} 最近失败率较高 (${failed.recentFailures}/${this.config.recentWindow})`);
    }
    if (suggested.successRate > failed.successRate) {
      reasons.push(`${suggested.cli} 成功率更高 (${(suggested.successRate * 100).toFixed(0)}%)`);
    }
    if (suggested.avgDuration < failed.avgDuration && failed.avgDuration > 0) {
      reasons.push(`${suggested.cli} 平均响应更快`);
    }

    return reasons.join('；') || `${suggested.cli} 当前状态更稳定`;
  }

  /** 计算降级建议的置信度 */
  private calculateConfidence(stats: CLIStats): number {
    const sampleFactor = Math.min(stats.totalExecutions / 10, 1);
    const successFactor = stats.successRate;
    const recentFactor = 1 - (stats.recentFailures / this.config.recentWindow);
    return (sampleFactor * 0.3 + successFactor * 0.4 + recentFactor * 0.3);
  }

  /** 从存储加载数据 */
  private loadFromStorage(): void {
    if (!this.context) return;

    try {
      const data = this.context.globalState.get<ExecutionRecord[]>(this.config.persistKey);
      if (data && Array.isArray(data)) {
        this.records = data;
        console.log(`[ExecutionStats] 从存储加载 ${this.records.length} 条记录`);
      }
    } catch (error) {
      console.warn('[ExecutionStats] 加载存储数据失败:', error);
    }
  }

  /** 保存数据到存储 */
  private async saveToStorage(): Promise<void> {
    if (!this.context) return;

    try {
      await this.context.globalState.update(this.config.persistKey, this.records);
    } catch (error) {
      console.warn('[ExecutionStats] 保存存储数据失败:', error);
    }
  }

  /** 清除所有统计数据 */
  async clearStats(): Promise<void> {
    this.records = [];
    await this.saveToStorage();
    console.log('[ExecutionStats] 已清除所有统计数据');
    globalEventBus.emitEvent('execution:stats_updated', {});
  }

  /** 获取统计摘要（用于 UI 显示） */
  getSummary(): string {
    const allStats = this.getAllStats();
    return allStats.map(s =>
      `${s.cli}: ${(s.successRate * 100).toFixed(0)}% (${s.totalExecutions}次)`
    ).join(' | ');
  }
}
