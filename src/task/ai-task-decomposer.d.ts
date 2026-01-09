/**
 * AI 任务分解器
 * 调用 AI CLI 分析复杂任务并自动分解为子任务
 */
import { CLIType } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { TaskAnalysis } from './task-analyzer';
import { SplitResult } from './task-splitter';
import { CLISelector } from './cli-selector';
/** AI 分解配置 */
export interface AIDecomposeConfig {
    /** 用于分解任务的 CLI（默认 claude） */
    decomposeCli: CLIType;
    /** 复杂度阈值，超过此值才使用 AI 分解 */
    complexityThreshold: number;
    /** 超时时间（毫秒） */
    timeout: number;
}
/**
 * AI 任务分解器
 */
export declare class AITaskDecomposer {
    private cliFactory;
    private cliSelector;
    private config;
    constructor(cliFactory: CLIAdapterFactory, cliSelector: CLISelector, config?: Partial<AIDecomposeConfig>);
    /**
     * 判断是否需要 AI 分解
     */
    shouldUseAI(analysis: TaskAnalysis): boolean;
    /**
     * 使用 AI 分解任务
     */
    decompose(analysis: TaskAnalysis): Promise<SplitResult>;
    /**
     * 构建分解提示词
     */
    private buildDecomposePrompt;
    /**
     * 解析 AI 响应
     */
    private parseAIResponse;
    /**
     * 标准化任务类型
     */
    private normalizeCategory;
    /**
     * 确定执行模式
     */
    private determineExecutionMode;
    /**
     * 降级到规则分解
     */
    private fallbackSplit;
    /**
     * 更新配置
     */
    updateConfig(config: Partial<AIDecomposeConfig>): void;
}
//# sourceMappingURL=ai-task-decomposer.d.ts.map