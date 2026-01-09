/**
 * Claude Worker
 * Claude CLI 执行器
 */
import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { BaseWorker } from './base-worker';
/**
 * Claude Worker 配置
 */
export interface ClaudeWorkerConfig extends WorkerConfig {
    model?: string;
    maxTokens?: number;
}
/**
 * Claude CLI Worker
 */
export declare class ClaudeWorker extends BaseWorker {
    private claudeConfig;
    constructor(config: ClaudeWorkerConfig);
    get cliType(): CLIType;
    /** 构建 Claude CLI 命令参数 */
    protected buildArgs(subTask: SubTask): string[];
    /** 构建提示词 */
    private buildPrompt;
    /** 解析 Claude CLI 输出 */
    protected parseOutput(output: string): Partial<WorkerResult>;
}
/** 创建 Claude Worker 的工厂函数 */
export declare function createClaudeWorker(cliPath: string | undefined, workingDirectory: string, timeout?: number): ClaudeWorker;
//# sourceMappingURL=claude-worker.d.ts.map