/**
 * Gemini Worker
 * Gemini CLI 执行器
 */
import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { BaseWorker } from './base-worker';
/**
 * Gemini Worker 配置
 */
export interface GeminiWorkerConfig extends Omit<WorkerConfig, 'sandbox'> {
    model?: string;
    sandbox?: boolean;
}
/**
 * Gemini CLI Worker
 */
export declare class GeminiWorker extends BaseWorker {
    private geminiConfig;
    constructor(config: GeminiWorkerConfig);
    get cliType(): CLIType;
    /** 构建 Gemini CLI 命令参数 */
    protected buildArgs(subTask: SubTask): string[];
    /** 构建提示词 */
    private buildPrompt;
    /** 解析 Gemini CLI 输出 */
    protected parseOutput(output: string): Partial<WorkerResult>;
}
/** 创建 Gemini Worker 的工厂函数 */
export declare function createGeminiWorker(cliPath: string | undefined, workingDirectory: string, timeout?: number): GeminiWorker;
//# sourceMappingURL=gemini-worker.d.ts.map