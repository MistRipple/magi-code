/**
 * Orchestrator - 核心编排器
 * 负责任务分解、Worker 调度、结果收集
 */
import { CLIType, ExecutionMode } from './types';
import { SessionManager } from './session-manager';
import { TaskManager } from './task-manager';
import { SnapshotManager } from './snapshot-manager';
import { BaseWorker } from './workers/base-worker';
/** Orchestrator 配置 */
export interface OrchestratorOptions {
    workspaceRoot: string;
    sessionManager: SessionManager;
    taskManager: TaskManager;
    snapshotManager: SnapshotManager;
    mode?: ExecutionMode;
    timeout?: number;
}
/**
 * Orchestrator 编排器
 */
export declare class Orchestrator {
    private options;
    private cliDetector;
    private workers;
    private isRunning;
    constructor(options: OrchestratorOptions);
    /** 初始化 Workers */
    private initWorkers;
    /** 执行任务 */
    executeTask(taskId: string): Promise<void>;
    private categorizeTask;
    private selectBestCLI;
    private extractTargetFiles;
    private executeSubTasks;
    private executeSubTask;
    interrupt(): void;
    get running(): boolean;
    getWorker(cli: CLIType): BaseWorker | undefined;
}
//# sourceMappingURL=orchestrator.d.ts.map