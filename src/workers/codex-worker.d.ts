/**
 * Codex Worker
 * Codex CLI 执行器
 */
import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { BaseWorker } from './base-worker';
/**
 * Codex Worker 配置
 */
export interface CodexWorkerConfig extends WorkerConfig {
    model?: string;
    approval?: 'suggest' | 'auto-edit' | 'full-auto';
}
/**
 * Codex CLI Worker
 */
export declare class CodexWorker extends BaseWorker {
    private codexConfig;
    constructor(config: CodexWorkerConfig);
    get cliType(): CLIType;
    /** 构建 Codex CLI 命令参数 */
    protected buildArgs(subTask: SubTask): string[];
    /** 构建提示词 */
    private buildPrompt;
    /** 解析 Codex CLI 输出 */
    protected parseOutput(output: string): Partial<WorkerResult>;
}
/** 创建 Codex Worker 的工厂函数 */
export declare function createCodexWorker(cliPath: string | undefined, workingDirectory: string, timeout?: number): CodexWorker;
//# sourceMappingURL=codex-worker.d.ts.map