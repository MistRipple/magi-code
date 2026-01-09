/**
 * Diff 生成器
 * 本地 Diff 生成，对比快照与当前文件
 */
import { DiffHunk, CLIType } from './types';
import { SessionManager } from './session-manager';
/**
 * Diff 结果
 */
export interface DiffResult {
    filePath: string;
    hunks: DiffHunk[];
    additions: number;
    deletions: number;
    source: CLIType;
}
/**
 * Diff 生成器
 */
export declare class DiffGenerator {
    private sessionManager;
    private workspaceRoot;
    constructor(sessionManager: SessionManager, workspaceRoot: string);
    /** 生成文件 Diff */
    generateDiff(filePath: string): DiffResult | null;
    /** 计算 Diff Hunks */
    private computeHunks;
    /** 创建 DiffHunk */
    private createHunk;
    /** 简单 Diff 算法 */
    private simpleDiff;
    /** 生成所有待处理文件的 Diff */
    generateAllDiffs(): DiffResult[];
    /** 格式化 Diff 为统一格式字符串 */
    formatDiff(diff: DiffResult): string;
}
//# sourceMappingURL=diff-generator.d.ts.map