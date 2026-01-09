"use strict";
/**
 * 结果聚合器
 * 合并多个 CLI 的输出，生成统一报告
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.ResultAggregator = void 0;
/**
 * 结果聚合器类
 */
class ResultAggregator {
    /**
     * 聚合执行结果
     */
    aggregate(results, diffs) {
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
    computeTaskStats(results) {
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
    computeCLIStats(results) {
        const stats = {
            claude: { taskCount: 0, successCount: 0, failedCount: 0, totalDuration: 0 },
            codex: { taskCount: 0, successCount: 0, failedCount: 0, totalDuration: 0 },
            gemini: { taskCount: 0, successCount: 0, failedCount: 0, totalDuration: 0 },
        };
        for (const result of results) {
            const cli = result.cli;
            stats[cli].taskCount++;
            if (result.status === 'completed')
                stats[cli].successCount++;
            if (result.status === 'failed')
                stats[cli].failedCount++;
            stats[cli].totalDuration += result.duration || 0;
        }
        return stats;
    }
    /**
     * 计算文件统计
     */
    computeFileStats(diffs) {
        return {
            total: diffs.length,
            additions: diffs.reduce((sum, d) => sum + d.additions, 0),
            deletions: diffs.reduce((sum, d) => sum + d.deletions, 0),
        };
    }
    /**
     * 提取文件变更
     */
    extractFileChanges(diffs) {
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
    determineStatus(stats) {
        if (stats.failed === 0 && stats.cancelled === 0)
            return 'success';
        if (stats.completed > 0)
            return 'partial';
        return 'failed';
    }
    /**
     * 生成摘要文本
     */
    generateSummary(taskStats, cliStats, fileStats, totalDuration) {
        const lines = [];
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
exports.ResultAggregator = ResultAggregator;
//# sourceMappingURL=result-aggregator.js.map