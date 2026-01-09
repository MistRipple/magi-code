/**
 * Orchestrator Agent - 独立编排者 Claude
 *
 * 核心职责：
 * - 专职编排，不执行任何编码任务
 * - 实现事件循环，实时监控所有 Worker
 * - 响应用户交互和 Worker 反馈
 * - 动态调度和错误处理
 * - 🆕 CLI 降级和执行统计
 *
 * 架构理念：
 * - 编排者是"永远在线"的协调者
 * - 100% 时间用于监控和协调
 * - 可以立即响应任何事件
 */
import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { ExecutionStats } from './execution-stats';
import { SnapshotManager } from '../snapshot-manager';
import { TaskManager } from '../task-manager';
import { OrchestratorState, OrchestratorConfig, ExecutionPlan, TaskContext } from './protocols/types';
/** 用户确认回调类型 */
export type ConfirmationCallback = (plan: ExecutionPlan, formattedPlan: string) => Promise<boolean>;
/**
 * Orchestrator Agent
 * 独立编排者 Claude 的核心实现
 * 🆕 集成 CLI 降级和执行统计
 */
export declare class OrchestratorAgent extends EventEmitter {
    readonly id: string;
    private cliFactory;
    private messageBus;
    private workerPool;
    private config;
    private verificationRunner;
    private workspaceRoot;
    private contextManager;
    private contextCompressor;
    private snapshotManager;
    private taskManager;
    private executionStats;
    private _state;
    private currentContext;
    private confirmationCallback;
    private abortController;
    private unsubscribers;
    private pendingTasks;
    private completedResults;
    constructor(cliFactory: CLIAdapterFactory, config?: Partial<OrchestratorConfig>, workspaceRoot?: string, snapshotManager?: SnapshotManager, taskManager?: TaskManager);
    /** 获取当前状态 */
    get state(): OrchestratorState;
    /** 获取当前任务上下文 */
    get context(): TaskContext | null;
    /** 设置状态 */
    private setState;
    /** 设置确认回调 */
    setConfirmationCallback(callback: ConfirmationCallback): void;
    /** 🆕 设置扩展上下文（用于持久化执行统计） */
    setExtensionContext(context: import('vscode').ExtensionContext): void;
    /** 🆕 获取执行统计实例 */
    getExecutionStats(): ExecutionStats;
    /** 🆕 获取执行统计摘要（用于 UI 显示） */
    getStatsSummary(): string;
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
    /**
     * 保存 Memory 并检查是否需要压缩
     */
    private saveAndCompressMemory;
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
    /** 🆕 基于依赖图分发任务 */
    private dispatchWithDependencyGraph;
    /** 为执行计划中的目标文件创建快照 */
    private createSnapshotsForPlan;
    /** 并行分发任务 */
    private dispatchParallel;
    /** 串行分发任务 */
    private dispatchSequential;
    /** 处理子任务执行结果 */
    private finalizeResult;
    private resolveReviewConfig;
    private shouldEnableReviews;
    private shouldPeerReview;
    private selectPeerReviewer;
    private buildSelfCheckPrompt;
    private buildPeerReviewPrompt;
    private parseReviewDecision;
    private runSubTaskReviews;
    private waitForAllFinalized;
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
    /** 处理进度汇报消息 */
    private handleProgressReport;
    /** 发送 UI 消息（标识来源为编排者） */
    private emitUIMessage;
    /** 销毁编排者 */
    dispose(): void;
}
//# sourceMappingURL=orchestrator-agent.d.ts.map
