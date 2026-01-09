/**
 * Worker Agent - 执行者代理基类
 *
 * 核心职责：
 * - 接收编排者分配的任务
 * - 执行编码任务
 * - 向编排者汇报进度和结果
 *
 * 所有 Worker（包括 Worker Claude）都继承此基类
 */
import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { CLIResponse } from '../cli/types';
import { MessageBus } from './message-bus';
import { WorkerType, WorkerState, WorkerInfo, SubTask, ExecutionResult } from './protocols/types';
/** Worker 配置 */
export interface WorkerConfig {
    type: WorkerType;
    cliFactory: CLIAdapterFactory;
    messageBus?: MessageBus;
    orchestratorId?: string;
}
/**
 * Worker Agent 基类
 * 封装 CLI 适配器，提供统一的任务执行和汇报接口
 */
export declare class WorkerAgent extends EventEmitter {
    readonly id: string;
    readonly type: WorkerType;
    protected cliFactory: CLIAdapterFactory;
    protected messageBus: MessageBus;
    protected orchestratorId: string;
    private _state;
    private currentTaskId;
    private currentSubTaskId;
    private abortController;
    private unsubscribers;
    constructor(config: WorkerConfig);
    /** 获取当前状态 */
    get state(): WorkerState;
    /** 获取 Worker 信息 */
    get info(): WorkerInfo;
    /** 设置状态 */
    protected setState(state: WorkerState): void;
    /** 设置消息处理器 */
    private setupMessageHandlers;
    /** 处理消息 */
    private handleMessage;
    /** 处理任务分发 */
    private handleTaskDispatch;
    /** 处理任务取消 */
    private handleTaskCancel;
    /** 处理编排者命令 */
    private handleOrchestratorCommand;
    /**
     * 执行任务
     * 核心方法：接收子任务，调用 CLI 执行，汇报结果
     */
    executeTask(taskId: string, subTask: SubTask, context?: string): Promise<ExecutionResult>;
    /**
     * 构建执行 prompt
     */
    protected buildExecutionPrompt(subTask: SubTask, context?: string): string;
    /**
     * 调用 CLI 执行任务
     */
    protected executeCLI(prompt: string): Promise<CLIResponse>;
    /**
     * 取消当前任务
     */
    cancel(): Promise<void>;
    /**
     * 汇报 Worker 就绪状态
     */
    reportReady(): void;
    /**
     * 清理任务状态
     */
    private cleanup;
    /**
     * 销毁 Worker
     */
    dispose(): void;
}
//# sourceMappingURL=worker-agent.d.ts.map