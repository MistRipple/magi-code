/**
 * 执行调度器
 * 支持并行/串行执行策略，管理任务队列
 */
import { EventEmitter } from 'events';
import { CLIType } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { CLIResponse } from '../cli/types';
import { SubTaskDef, SplitResult } from './task-splitter';
/** 执行状态 */
export type ExecutionStatus = 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
/** 子任务执行结果 */
export interface SubTaskResult {
    subTaskId: string;
    cli: CLIType;
    status: ExecutionStatus;
    response?: CLIResponse;
    error?: string;
    startTime: number;
    endTime?: number;
    duration?: number;
}
/** 调度器配置 */
export interface SchedulerConfig {
    maxParallel: number;
    timeout: number;
    retryCount: number;
}
/** 执行快照（用于持久化和恢复） */
export interface ExecutionSnapshot {
    queue: SubTaskDef[];
    results: SubTaskResult[];
    isRunning: boolean;
    isCancelled: boolean;
    timestamp: number;
}
/**
 * 执行调度器类
 */
export declare class ExecutionScheduler extends EventEmitter {
    private factory;
    private config;
    private queue;
    private running;
    private results;
    private isRunning;
    private isCancelled;
    constructor(factory: CLIAdapterFactory, config?: Partial<SchedulerConfig>);
    /**
     * 执行拆分结果
     */
    execute(splitResult: SplitResult): Promise<SubTaskResult[]>;
    /**
     * 串行执行
     */
    private executeSequential;
    /**
     * 并行执行
     */
    private executeParallel;
    /**
     * 查找可执行的任务
     */
    private findReadyTask;
    /**
     * 检查依赖是否满足
     */
    private checkDependencies;
    /**
     * 执行单个子任务（带重试机制）
     */
    private executeSubTask;
    /**
     * 带超时的执行
     */
    private executeWithTimeout;
    /**
     * 判断是否应该重试
     */
    private shouldRetry;
    /**
     * 计算重试延迟（指数退避）
     */
    private getRetryDelay;
    /**
     * 延迟函数
     */
    private delay;
    /** 取消执行 */
    cancel(): void;
    /** 获取可恢复的任务 */
    getResumableTasks(): SubTaskDef[];
    /** 恢复执行 */
    resume(): Promise<SubTaskResult[]>;
    /** 获取执行快照（用于持久化） */
    getSnapshot(): ExecutionSnapshot;
    /** 从快照恢复 */
    restoreFromSnapshot(snapshot: ExecutionSnapshot): void;
    /** 重置状态 */
    private reset;
    /** 获取执行状态 */
    get status(): {
        running: boolean;
        cancelled: boolean;
        pending: number;
        completed: number;
    };
}
//# sourceMappingURL=execution-scheduler.d.ts.map