/**
 * 智能编排器 - 独立编排者架构
 *
 * 架构重构：
 * - Orchestrator Claude：专职编排，不执行任何编码任务
 * - Worker Agents：专职执行，向编排者汇报进度和结果
 */
import { CLIType, InteractionMode } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { TaskManager } from '../task-manager';
import { SnapshotManager } from '../snapshot-manager';
import { ConfirmationCallback } from './orchestrator-agent';
import { VerificationConfig } from './verification-runner';
import { ExecutionPlan, ExecutionResult, SubTask, OrchestratorState } from './protocols/types';
export type { ExecutionPlan, ExecutionResult, SubTask };
export { ConfirmationCallback };
/** 子任务计划（向后兼容） */
export interface SubTaskPlan {
    id: string;
    description: string;
    assignedCli: CLIType;
    reason: string;
    targetFiles?: string[];
    dependencies: string[];
    prompt: string;
}
/** 编排器配置 */
export interface OrchestratorConfig {
    timeout: number;
    verification?: Partial<VerificationConfig>;
    maxRetries: number;
}
/** 编排器状态 */
export type OrchestratorPhase = OrchestratorState;
/** 恢复确认回调类型 */
export type RecoveryConfirmationCallback = (failedTask: any, error: string, options: {
    retry: boolean;
    rollback: boolean;
}) => Promise<'retry' | 'rollback' | 'continue'>;
/**
 * 智能编排器 - 基于独立编排者架构
 */
export declare class IntelligentOrchestrator {
    private cliFactory;
    private taskManager;
    private snapshotManager;
    private config;
    private workspaceRoot;
    private orchestratorAgent;
    private interactionMode;
    private modeConfig;
    private verificationRunner;
    private isRunning;
    private currentTaskId;
    private abortController;
    private statusUpdateInterval;
    constructor(cliFactory: CLIAdapterFactory, taskManager: TaskManager, snapshotManager: SnapshotManager, workspaceRoot: string, config?: Partial<OrchestratorConfig>);
    /** 设置编排者事件监听 */
    private setupOrchestratorEvents;
    /** 设置交互模式 */
    setInteractionMode(mode: InteractionMode): void;
    /** 获取当前交互模式 */
    getInteractionMode(): InteractionMode;
    /** 设置用户确认回调 */
    setConfirmationCallback(callback: ConfirmationCallback): void;
    /** 设置恢复确认回调（向后兼容） */
    setRecoveryConfirmationCallback(_callback: RecoveryConfirmationCallback): void;
    /** 获取当前阶段 */
    get phase(): OrchestratorPhase;
    /** 获取当前执行计划 */
    get plan(): ExecutionPlan | null;
    /** 是否正在运行（向后兼容） */
    get running(): boolean;
    /** 中断当前任务（向后兼容） */
    interrupt(): Promise<void>;
    /** 初始化编排者 */
    initialize(): Promise<void>;
    /**
     * 执行任务 - 主入口
     */
    execute(userPrompt: string, taskId: string): Promise<string>;
    /** ask 模式：仅对话 */
    private executeAskMode;
    /** 取消当前任务 */
    cancel(): Promise<void>;
    /** 开始状态更新定时器 */
    private startStatusUpdates;
    /** 停止状态更新定时器 */
    private stopStatusUpdates;
    /** 获取可用的 CLI 列表 */
    getAvailableCLIs(): CLIType[];
    /** 销毁编排器 */
    dispose(): void;
}
//# sourceMappingURL=intelligent-orchestrator.d.ts.map