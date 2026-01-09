/**
 * Orchestrator Agent - 独立编排者 Claude
 *
 * 核心职责：
 * - 专职编排，不执行任何编码任务
 * - 实现事件循环，实时监控所有 Worker
 * - 响应用户交互和 Worker 反馈
 * - 动态调度和错误处理
 *
 * 架构理念：
 * - 编排者是"永远在线"的协调者
 * - 100% 时间用于监控和协调
 * - 可以立即响应任何事件
 */
import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { OrchestratorState, OrchestratorConfig, ExecutionPlan, TaskContext } from './protocols/types';
/** 用户确认回调类型 */
export type ConfirmationCallback = (plan: ExecutionPlan, formattedPlan: string) => Promise<boolean>;
/**
 * Orchestrator Agent
 * 独立编排者 Claude 的核心实现
 */
export declare class OrchestratorAgent extends EventEmitter {
    readonly id: string;
    private cliFactory;
    private messageBus;
    private workerPool;
    private config;
    private verificationRunner;
    private workspaceRoot;
    private _state;
    private currentContext;
    private confirmationCallback;
    private abortController;
    private unsubscribers;
    private pendingTasks;
    private completedResults;
    private failedTasks;
    constructor(cliFactory: CLIAdapterFactory, config?: Partial<OrchestratorConfig>, workspaceRoot?: string);
    /** 获取当前状态 */
    get state(): OrchestratorState;
    /** 获取当前任务上下文 */
    get context(): TaskContext | null;
    /** 设置状态 */
    private setState;
    /** 设置确认回调 */
    setConfirmationCallback(callback: ConfirmationCallback): void;
    /** 初始化 */
    initialize(): Promise<void>;
    /** 设置消息处理器 */
    private setupMessageHandlers;
    /** 设置 Worker Pool 事件处理 */
    private setupWorkerPoolHandlers;
    /**
     * 执行任务 - 主入口
     */
    execute(userPrompt: string, taskId: string): Promise<string>;
    /** 检查是否被中断 */
    private checkAborted;
    /** 取消当前任务 */
    cancel(): Promise<void>;
    /** 清理状态 */
    private cleanup;
    /**
     * 分析任务，生成执行计划
     */
    private analyzeTask;
    /**
     * 解析执行计划 JSON
     */
    private parseExecutionPlan;
    /**
     * 等待用户确认执行计划
     */
    private waitForConfirmation;
    /** 分发任务给 Worker */
    private dispatchTasks;
    /** 并行分发任务 */
    private dispatchParallel;
    /** 串行分发任务 */
    private dispatchSequential;
    /** 监控任务执行（用于并行模式） */
    private monitorExecution;
    /** 执行验证 */
    private runVerification;
    /** 汇总执行结果 */
    private summarizeResults;
    /** 处理任务完成消息 */
    private handleTaskCompleted;
    /** 处理任务失败消息 */
    private handleTaskFailed;
    /** 重试失败的任务 */
    private retryTask;
    /** 处理进度汇报消息 */
    private handleProgressReport;
    /** 发送 UI 消息 */
    private emitUIMessage;
    /** 销毁编排者 */
    dispose(): void;
}
//# sourceMappingURL=orchestrator-agent.d.ts.map