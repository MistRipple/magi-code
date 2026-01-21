/**
 * 结果聚合器
 * 合并多个 CLI 的输出，生成统一报告
 */

import { CLIType, AgentType } from '../types';  // ✅ 导入 AgentType
import { SubTaskResult } from './execution-scheduler';
import { DiffResult } from '../diff-generator';

/** 聚合报告 */
export interface AggregatedReport {
  /** 总体状态 */
  status: 'success' | 'partial' | 'failed';
  /** 总耗时 (ms) */
  totalDuration: number;
  /** 子任务统计 */
  taskStats: {
    total: number;
    completed: number;
    failed: number;
    cancelled: number;
  };
  /** CLI 统计 */
  cliStats: Record<CLIType, CLIStats>;
  /** 文件变更统计 */
  fileStats: {
    total: number;
    additions: number;
    deletions: number;
  };
  /** 子任务结果列表 */
  results: SubTaskResult[];
  /** 文件变更列表 */
  fileChanges: FileChangeSummary[];
  /** 生成时间 */
  generatedAt: number;
  /** 摘要文本 */
  summary: string;
}

/** CLI 统计 */
export interface CLIStats {
  taskCount: number;
  successCount: number;
  failedCount: number;
  totalDuration: number;
}

/** 文件变更摘要 */
export interface FileChangeSummary {
  filePath: string;
  cli: AgentType;  // ✅ 使用 AgentType
  additions: number;
  deletions: number;
}

/**
 * 结果聚合器类
 */
export class ResultAggregator {
  /**
   * 聚合执行结果
   */
  aggregate(results: SubTaskResult[], diffs?: DiffResult[]): AggregatedReport {
    const taskStats = this.computeTaskStats(results);
    const cliStats = this.computeCLIStats(results);
    const fileStats = this.computeFileStats(diffs || []);
    const fileChanges = this.extractFileChanges(diffs || []);
    const totalDuration = results.reduce((sum, r) => sum + (r.duration || 0), 0);

    const status = this.determineStatus(taskStats);
    const summary = this.generateSummary(taskStats, cliStats, fileStats, totalDuration);

    return {
      status,
      totalDuration,
      taskStats,
      cliStats,
      fileStats,
      results,
      fileChanges,
      generatedAt: Date.now(),
      summary,
    };
  }

  /**
   * 计算任务统计
   */
  private computeTaskStats(results: SubTaskResult[]): AggregatedReport['taskStats'] {
    return {
      total: results.length,
      completed: results.filter(r => r.status === 'completed').length,
      failed: results.filter(r => r.status === 'failed').length,
      cancelled: results.filter(r => r.status === 'cancelled').length,
    };
  }

  /**
   * 计算 CLI 统计
   */
  private computeCLIStats(results: SubTaskResult[]): Record<CLIType, CLIStats> {
    const stats: Record<CLIType, CLIStats> = {
      claude: { taskCount: 0, successCount: 0, failedCount: 0, totalDuration: 0 },
      codex: { taskCount: 0, successCount: 0, failedCount: 0, totalDuration: 0 },
      gemini: { taskCount: 0, successCount: 0, failedCount: 0, totalDuration: 0 },
    };

    for (const result of results) {
      const cli = result.cli;
      stats[cli].taskCount++;
      if (result.status === 'completed') stats[cli].successCount++;
      if (result.status === 'failed') stats[cli].failedCount++;
      stats[cli].totalDuration += result.duration || 0;
    }

    return stats;
  }

  /**
   * 计算文件统计
   */
  private computeFileStats(diffs: DiffResult[]): AggregatedReport['fileStats'] {
    return {
      total: diffs.length,
      additions: diffs.reduce((sum, d) => sum + d.additions, 0),
      deletions: diffs.reduce((sum, d) => sum + d.deletions, 0),
    };
  }

  /**
   * 提取文件变更
   */
  private extractFileChanges(diffs: DiffResult[]): FileChangeSummary[] {
    return diffs.map(d => ({
      filePath: d.filePath,
      cli: d.source,
      additions: d.additions,
      deletions: d.deletions,
    }));
  }

  /**
   * 确定总体状态
   */
  private determineStatus(stats: AggregatedReport['taskStats']): AggregatedReport['status'] {
    if (stats.failed === 0 && stats.cancelled === 0) return 'success';
    if (stats.completed > 0) return 'partial';
    return 'failed';
  }

  /**
   * 生成摘要文本
   */
  private generateSummary(
    taskStats: AggregatedReport['taskStats'],
    cliStats: Record<CLIType, CLIStats>,
    fileStats: AggregatedReport['fileStats'],
    totalDuration: number
  ): string {
    const lines: string[] = [];
    const durationSec = (totalDuration / 1000).toFixed(1);

    lines.push(`执行完成，耗时 ${durationSec}s`);
    lines.push(`任务: ${taskStats.completed}/${taskStats.total} 成功`);

    if (taskStats.failed > 0) {
      lines.push(`失败: ${taskStats.failed} 个任务`);
    }

    const usedCLIs = Object.entries(cliStats)
      .filter(([, s]) => s.taskCount > 0)
      .map(([cli, s]) => `${cli}(${s.successCount}/${s.taskCount})`);
    
    if (usedCLIs.length > 0) {
      lines.push(`CLI: ${usedCLIs.join(', ')}`);
    }

    if (fileStats.total > 0) {
      lines.push(`文件: ${fileStats.total} 个变更 (+${fileStats.additions}/-${fileStats.deletions})`);
    }

    return lines.join('\n');
  }
}

