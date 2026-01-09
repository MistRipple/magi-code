/**
 * 结果聚合器
 * 合并多个 CLI 的输出，生成统一报告
 */
import { CLIType } from '../types';
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
    cli: CLIType;
    additions: number;
    deletions: number;
}
/**
 * 结果聚合器类
 */
export declare class ResultAggregator {
    /**
     * 聚合执行结果
     */
    aggregate(results: SubTaskResult[], diffs?: DiffResult[]): AggregatedReport;
    /**
     * 计算任务统计
     */
    private computeTaskStats;
    /**
     * 计算 CLI 统计
     */
    private computeCLIStats;
    /**
     * 计算文件统计
     */
    private computeFileStats;
    /**
     * 提取文件变更
     */
    private extractFileChanges;
    /**
     * 确定总体状态
     */
    private determineStatus;
    /**
     * 生成摘要文本
     */
    private generateSummary;
}
//# sourceMappingURL=result-aggregator.d.ts.map