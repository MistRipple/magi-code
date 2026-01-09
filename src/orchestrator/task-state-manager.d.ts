/**
 * 任务状态管理器
 * 负责追踪所有子任务的执行状态，支持持久化和实时同步
 */
import { CLIType } from '../types';
/** 任务状态类型 */
export type TaskStatus = 'pending' | 'running' | 'completed' | 'failed' | 'retrying' | 'cancelled';
/** 任务状态 */
export interface TaskState {
    id: string;
    parentTaskId: string;
    description: string;
    assignedCli: CLIType;
    status: TaskStatus;
    progress: number;
    attempts: number;
    maxAttempts: number;
    startedAt?: number;
    completedAt?: number;
    result?: string;
    error?: string;
    modifiedFiles?: string[];
}
/** 状态变更回调 */
export type StateChangeCallback = (task: TaskState, allTasks: TaskState[]) => void;
/**
 * 任务状态管理器
 */
export declare class TaskStateManager {
    private tasks;
    private sessionId;
    private workspaceRoot;
    private callbacks;
    private autoSave;
    constructor(sessionId: string, workspaceRoot: string, autoSave?: boolean);
    /** 创建新任务 */
    createTask(params: {
        id: string;
        parentTaskId: string;
        description: string;
        assignedCli: CLIType;
        maxAttempts?: number;
    }): TaskState;
    /** 更新任务状态 */
    updateStatus(taskId: string, status: TaskStatus, error?: string): void;
    /** 更新任务进度 */
    updateProgress(taskId: string, progress: number): void;
    /** 设置任务结果 */
    setResult(taskId: string, result: string, modifiedFiles?: string[]): void;
    /** 获取单个任务 */
    getTask(taskId: string): TaskState | null;
    /** 获取所有任务 */
    getAllTasks(): TaskState[];
    /** 获取待执行的任务 */
    getPendingTasks(cli?: CLIType): TaskState[];
    /** 获取指定 CLI 的任务 */
    getTasksByCli(cli: CLIType): TaskState[];
    /** 检查是否所有任务都已完成 */
    isAllCompleted(): boolean;
    /** 检查是否有失败的任务 */
    hasFailedTasks(): boolean;
    /** 获取失败的任务 */
    getFailedTasks(): TaskState[];
    /** 检查任务是否可以重试 */
    canRetry(taskId: string): boolean;
    /** 重置任务为待执行状态（用于重试） */
    resetForRetry(taskId: string): void;
    /** 注册状态变更回调 */
    onStateChange(callback: StateChangeCallback): () => void;
    /** 通知状态变更 */
    private notifyChange;
    /** 自动保存（如果启用） */
    private autoSaveIfEnabled;
    /** 获取存储路径 */
    private getStoragePath;
    /** 保存到文件 */
    save(): Promise<void>;
    /** 从文件加载 */
    load(): Promise<void>;
    /** 清除所有任务 */
    clear(): void;
    /** 获取统计信息 */
    getStats(): {
        total: number;
        pending: number;
        running: number;
        completed: number;
        failed: number;
        cancelled: number;
    };
}
//# sourceMappingURL=task-state-manager.d.ts.map