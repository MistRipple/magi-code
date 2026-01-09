/**
 * Worker Pool - Worker 池管理
 *
 * 核心功能：
 * - 管理所有 Worker 实例
 * - 提供 Worker 获取和分配
 * - 监控 Worker 状态
 * - CLI 降级和故障转移
 * - 🆕 任务依赖图调度
 */
import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { WorkerAgent } from './worker-agent';
import { MessageBus } from './message-bus';
import { ExecutionStats } from './execution-stats';
import { TaskDependencyGraph } from './task-dependency-graph';
import { WorkerType, WorkerState, WorkerInfo, SubTask, ExecutionResult } from './protocols/types';
/** Worker Pool 配置 */
export interface WorkerPoolConfig {
    cliFactory: CLIAdapterFactory;
    messageBus?: MessageBus;
    orchestratorId?: string;
    /** 执行调度配置 */
    scheduling?: SchedulingConfig;
    /** 执行统计实例（可选，用于 CLI 降级决策） */
    executionStats?: ExecutionStats;
    /** 是否启用 CLI 降级 */
    enableFallback?: boolean;
}
/** 执行调度配置 */
export interface SchedulingConfig {
    /** 最大并行任务数 */
    maxParallel: number;
    /** 任务超时时间 (ms) */
    timeout: number;
    /** 最大重试次数 */
    maxRetries: number;
    /** 重试基础延迟 (ms) */
    retryBaseDelay: number;
}
export interface DispatchOptions {
    priority?: number;
    abortSignal?: AbortSignal;
}
/** 任务执行状态 */
export interface TaskExecutionState {
    subTaskId: string;
    workerType: WorkerType;
    status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
    retries: number;
    startTime?: number;
    endTime?: number;
    error?: string;
}
/** Worker 状态变更事件 */
export interface WorkerStateChangeEvent {
    workerId: string;
    workerType: WorkerType;
    oldState: WorkerState;
    newState: WorkerState;
}
/**
 * Worker Pool
 * 管理所有 Worker 实例，提供统一的访问接口
 * 集成 ExecutionScheduler 的高级调度能力
 * 支持 CLI 降级和故障转移
 */
export declare class WorkerPool extends EventEmitter {
    private workers;
    private cliFactory;
    private messageBus;
    private orchestratorId;
    private unsubscribers;
    private schedulingConfig;
    private executionStates;
    private runningCount;
    private taskQueues;
    private queueProcessing;
    private queueCounter;
    private fileLockManager;
    private cancelGeneration;
    private executionStats?;
    private enableFallback;
    constructor(config: WorkerPoolConfig);
    /** 🆕 设置执行统计实例 */
    setExecutionStats(stats: ExecutionStats): void;
    /** 🆕 获取执行统计实例 */
    getExecutionStats(): ExecutionStats | undefined;
    /**
     * 初始化所有 Worker
     */
    initialize(): Promise<void>;
    /**
     * 创建单个 Worker
     */
    private createWorker;
    /**
     * 设置消息处理器
     */
    private setupMessageHandlers;
    /**
     * 获取指定类型的 Worker
     */
    getWorker(type: WorkerType): WorkerAgent | undefined;
    /**
     * 获取或创建 Worker
     */
    getOrCreateWorker(type: WorkerType): Promise<WorkerAgent>;
    /**
     * 获取所有 Worker
     */
    getAllWorkers(): WorkerAgent[];
    /**
     * 获取所有 Worker 信息
     */
    getAllWorkerInfo(): WorkerInfo[];
    /**
     * 获取空闲的 Worker
     */
    getIdleWorkers(): WorkerAgent[];
    /**
     * 获取指定类型的空闲 Worker
     */
    getIdleWorker(type: WorkerType): WorkerAgent | undefined;
    /**
     * 检查指定类型的 Worker 是否空闲
     */
    isWorkerIdle(type: WorkerType): boolean;
    /**
     * 分发任务给指定 Worker
     */
    dispatchTask(type: WorkerType, taskId: string, subTask: SubTask, context?: string, options?: DispatchOptions): Promise<ExecutionResult>;
    private getQueue;
    private enqueueTask;
    private processQueue;
    private findNextQueueItemIndex;
    private computeQueuePriority;
    private removeQueueItem;
    /**
     * 带重试、降级和超时的任务分发（集成 ExecutionScheduler 能力）
     * 🆕 支持 CLI 降级：当原 CLI 失败时，自动尝试其他可用 CLI
     */
    dispatchTaskWithRetry(type: WorkerType, taskId: string, subTask: SubTask, context?: string, options?: DispatchOptions): Promise<ExecutionResult>;
    /**
     * 带超时的任务执行
     */
    private executeWithTimeout;
    /**
     * 判断是否应该重试
     */
    private shouldRetry;
    /**
     * 计算重试延迟（指数退避 + 随机抖动）
     */
    private getRetryDelay;
    /**
     * 延迟函数
     */
    private delay;
    /**
     * 🆕 记录执行统计
     */
    private recordExecution;
    /**
     * 🆕 尝试 CLI 降级
     * @param failedCli 失败的 CLI
     * @param triedClis 已尝试过的 CLI 列表
     * @returns 降级建议，如果没有可用的降级选项则返回 null
     */
    private tryFallback;
    /**
     * 获取任务执行状态
     */
    getExecutionState(subTaskId: string): TaskExecutionState | undefined;
    /**
     * 获取当前运行中的任务数
     */
    getRunningCount(): number;
    /**
     * 清除执行状态
     */
    clearExecutionStates(): void;
    /**
     * 通过消息总线分发任务（异步）
     */
    dispatchTaskAsync(type: WorkerType, taskId: string, subTask: SubTask, context?: string): void;
    /**
     * 取消指定 Worker 的任务
     */
    cancelWorkerTask(type: WorkerType): Promise<void>;
    /**
     * 取消所有 Worker 的任务
     */
    cancelAllTasks(): Promise<void>;
    /**
     * 广播取消命令
     */
    broadcastCancel(): void;
    /**
     * 🆕 基于依赖图执行任务批次
     * 按照拓扑排序的批次顺序执行任务，同一批次内的任务并行执行
     */
    executeWithDependencyGraph(taskId: string, subTasks: SubTask[], context?: string): Promise<ExecutionResult[]>;
    /**
     * 🆕 并行执行一批任务
     */
    private executeBatchParallel;
    /**
     * 🆕 创建任务依赖图（供外部使用）
     */
    createDependencyGraph(subTasks: SubTask[]): TaskDependencyGraph;
    /**
     * 销毁 Worker Pool
     */
    dispose(): void;
}
//# sourceMappingURL=worker-pool.d.ts.map