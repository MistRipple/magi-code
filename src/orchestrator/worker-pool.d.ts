/**
 * Worker Pool - Worker 池管理
 *
 * 核心功能：
 * - 管理所有 Worker 实例
 * - 提供 Worker 获取和分配
 * - 监控 Worker 状态
 */
import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { WorkerAgent } from './worker-agent';
import { MessageBus } from './message-bus';
import { WorkerType, WorkerState, WorkerInfo, SubTask, ExecutionResult } from './protocols/types';
/** Worker Pool 配置 */
export interface WorkerPoolConfig {
    cliFactory: CLIAdapterFactory;
    messageBus?: MessageBus;
    orchestratorId?: string;
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
 */
export declare class WorkerPool extends EventEmitter {
    private workers;
    private cliFactory;
    private messageBus;
    private orchestratorId;
    private unsubscribers;
    constructor(config: WorkerPoolConfig);
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
    dispatchTask(type: WorkerType, taskId: string, subTask: SubTask, context?: string): Promise<ExecutionResult>;
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
     * 销毁 Worker Pool
     */
    dispose(): void;
}
//# sourceMappingURL=worker-pool.d.ts.map