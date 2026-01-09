/**
 * 失败恢复处理器
 * 实现 3-Strike Protocol，负责 Phase 5 的失败恢复
 */
import { CLIType } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { SnapshotManager } from '../snapshot-manager';
import { TaskStateManager, TaskState } from './task-state-manager';
import { VerificationResult } from './verification-runner';
/** 恢复策略 */
export type RecoveryStrategy = 'retry_same_cli' | 'retry_with_context' | 'escalate_to_claude' | 'rollback';
/** 恢复结果 */
export interface RecoveryResult {
    success: boolean;
    strategy: RecoveryStrategy;
    attempts: number;
    message: string;
    rolledBack?: boolean;
}
/** 恢复配置 */
export interface RecoveryConfig {
    maxAttempts: number;
    enableRollback: boolean;
    escalateCli: CLIType;
}
/**
 * 失败恢复处理器
 */
export declare class RecoveryHandler {
    private cliFactory;
    private snapshotManager;
    private taskStateManager;
    private config;
    constructor(cliFactory: CLIAdapterFactory, snapshotManager: SnapshotManager, taskStateManager: TaskStateManager, config?: Partial<RecoveryConfig>);
    /**
     * 执行恢复流程
     */
    recover(taskId: string, failedTask: TaskState, verificationResult: VerificationResult, errorDetails: string): Promise<RecoveryResult>;
    /**
     * 确定恢复策略
     */
    private determineStrategy;
    /**
     * Strike 1: 原 CLI 尝试修复
     */
    private retrySameCli;
    /**
     * Strike 2: 提供更多上下文重试
     */
    private retryWithContext;
    /**
     * Strike 3: 升级到 Claude 处理
     */
    private escalateToClaude;
    /**
     * 超过最大重试次数：执行回滚
     */
    private performRollback;
    /**
     * 构建修复 Prompt
     */
    private buildFixPrompt;
    /**
     * 构建升级到 Claude 的 Prompt
     */
    private buildEscalatePrompt;
    /**
     * 检查是否应该继续恢复
     */
    shouldContinueRecovery(task: TaskState): boolean;
    /**
     * 获取恢复统计
     */
    getRecoveryStats(tasks: TaskState[]): {
        totalRecoveries: number;
        successfulRecoveries: number;
        failedRecoveries: number;
        rollbacks: number;
    };
}
//# sourceMappingURL=recovery-handler.d.ts.map