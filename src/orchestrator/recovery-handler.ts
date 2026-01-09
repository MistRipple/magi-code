/**
 * 失败恢复处理器
 * 实现 3-Strike Protocol，负责 Phase 5 的失败恢复
 */

import { CLIType } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { SnapshotManager } from '../snapshot-manager';
import { globalEventBus } from '../events';
import { TaskStateManager, TaskState } from './task-state-manager';
import { VerificationResult } from './verification-runner';

/** 恢复策略 */
export type RecoveryStrategy = 
  | 'retry_same_cli'      // Strike 1: 原 CLI 修复
  | 'retry_with_context'  // Strike 2: 提供更多上下文
  | 'escalate_to_claude'  // Strike 3: 升级到 Claude
  | 'rollback';           // 超过 3 次: 回滚

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

const DEFAULT_CONFIG: RecoveryConfig = {
  maxAttempts: 3,
  enableRollback: true,
  escalateCli: 'claude',
};

/**
 * 失败恢复处理器
 */
export class RecoveryHandler {
  private cliFactory: CLIAdapterFactory;
  private snapshotManager: SnapshotManager;
  private taskStateManager: TaskStateManager;
  private config: RecoveryConfig;

  constructor(
    cliFactory: CLIAdapterFactory,
    snapshotManager: SnapshotManager,
    taskStateManager: TaskStateManager,
    config?: Partial<RecoveryConfig>
  ) {
    this.cliFactory = cliFactory;
    this.snapshotManager = snapshotManager;
    this.taskStateManager = taskStateManager;
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 执行恢复流程
   */
  async recover(
    taskId: string,
    failedTask: TaskState,
    verificationResult: VerificationResult,
    errorDetails: string
  ): Promise<RecoveryResult> {
    const attempts = failedTask.attempts;
    console.log(`[RecoveryHandler] 开始恢复流程，当前尝试次数: ${attempts}`);

    globalEventBus.emitEvent('recovery:started', {
      taskId,
      data: { attempts, maxAttempts: this.config.maxAttempts }
    });

    // 确定恢复策略
    const strategy = this.determineStrategy(attempts);
    console.log(`[RecoveryHandler] 选择策略: ${strategy}`);

    let result: RecoveryResult;

    switch (strategy) {
      case 'retry_same_cli':
        result = await this.retrySameCli(taskId, failedTask, errorDetails);
        break;
      case 'retry_with_context':
        result = await this.retryWithContext(taskId, failedTask, errorDetails);
        break;
      case 'escalate_to_claude':
        result = await this.escalateToClaude(taskId, failedTask, errorDetails);
        break;
      case 'rollback':
        result = await this.performRollback(taskId, failedTask);
        break;
      default:
        result = {
          success: false,
          strategy: 'rollback',
          attempts,
          message: '未知恢复策略',
        };
    }

    globalEventBus.emitEvent('recovery:completed', {
      taskId,
      data: result
    });

    return result;
  }

  /**
   * 确定恢复策略
   */
  private determineStrategy(attempts: number): RecoveryStrategy {
    if (attempts < 1) return 'retry_same_cli';
    if (attempts < 2) return 'retry_with_context';
    if (attempts < this.config.maxAttempts) return 'escalate_to_claude';
    return 'rollback';
  }

  /**
   * Strike 1: 原 CLI 尝试修复
   */
  private async retrySameCli(
    taskId: string,
    failedTask: TaskState,
    errorDetails: string
  ): Promise<RecoveryResult> {
    console.log(`[RecoveryHandler] Strike 1: ${failedTask.assignedCli} 尝试修复`);

    this.taskStateManager.resetForRetry(failedTask.id);

    const fixPrompt = this.buildFixPrompt(failedTask, errorDetails, 'simple');

    try {
      const response = await this.cliFactory.sendMessage(failedTask.assignedCli, fixPrompt);

      if (response.error) {
        return {
          success: false,
          strategy: 'retry_same_cli',
          attempts: failedTask.attempts,
          message: `修复失败: ${response.error}`,
        };
      }

      return {
        success: true,
        strategy: 'retry_same_cli',
        attempts: failedTask.attempts,
        message: '原 CLI 修复成功',
      };
    } catch (error) {
      return {
        success: false,
        strategy: 'retry_same_cli',
        attempts: failedTask.attempts,
        message: `修复异常: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
  }

  /**
   * Strike 2: 提供更多上下文重试
   */
  private async retryWithContext(
    taskId: string,
    failedTask: TaskState,
    errorDetails: string
  ): Promise<RecoveryResult> {
    console.log(`[RecoveryHandler] Strike 2: 提供更多上下文给 ${failedTask.assignedCli}`);

    this.taskStateManager.resetForRetry(failedTask.id);

    const fixPrompt = this.buildFixPrompt(failedTask, errorDetails, 'detailed');

    try {
      const response = await this.cliFactory.sendMessage(failedTask.assignedCli, fixPrompt);

      if (response.error) {
        return {
          success: false,
          strategy: 'retry_with_context',
          attempts: failedTask.attempts,
          message: `带上下文修复失败: ${response.error}`,
        };
      }

      return {
        success: true,
        strategy: 'retry_with_context',
        attempts: failedTask.attempts,
        message: '带上下文修复成功',
      };
    } catch (error) {
      return {
        success: false,
        strategy: 'retry_with_context',
        attempts: failedTask.attempts,
        message: `修复异常: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
  }

  /**
   * Strike 3: 升级到 Claude 处理
   */
  private async escalateToClaude(
    taskId: string,
    failedTask: TaskState,
    errorDetails: string
  ): Promise<RecoveryResult> {
    console.log(`[RecoveryHandler] Strike 3: 升级到 ${this.config.escalateCli}`);

    this.taskStateManager.resetForRetry(failedTask.id);

    const escalatePrompt = this.buildEscalatePrompt(failedTask, errorDetails);

    try {
      const response = await this.cliFactory.sendMessage(this.config.escalateCli, escalatePrompt);

      if (response.error) {
        return {
          success: false,
          strategy: 'escalate_to_claude',
          attempts: failedTask.attempts,
          message: `Claude 修复失败: ${response.error}`,
        };
      }

      return {
        success: true,
        strategy: 'escalate_to_claude',
        attempts: failedTask.attempts,
        message: 'Claude 修复成功',
      };
    } catch (error) {
      return {
        success: false,
        strategy: 'escalate_to_claude',
        attempts: failedTask.attempts,
        message: `Claude 修复异常: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
  }

  /**
   * 超过最大重试次数：执行回滚
   */
  private async performRollback(
    taskId: string,
    failedTask: TaskState
  ): Promise<RecoveryResult> {
    console.log(`[RecoveryHandler] 超过最大重试次数，执行回滚`);

    if (!this.config.enableRollback) {
      return {
        success: false,
        strategy: 'rollback',
        attempts: failedTask.attempts,
        message: '回滚已禁用，任务失败',
        rolledBack: false,
      };
    }

    try {
      // 获取该任务修改的文件
      const modifiedFiles = failedTask.modifiedFiles || [];

      for (const file of modifiedFiles) {
        this.snapshotManager.revertToSnapshot(file);
      }

      this.taskStateManager.updateStatus(failedTask.id, 'cancelled', '已回滚');

      return {
        success: false,
        strategy: 'rollback',
        attempts: failedTask.attempts,
        message: `任务失败，已回滚 ${modifiedFiles.length} 个文件`,
        rolledBack: true,
      };
    } catch (error) {
      return {
        success: false,
        strategy: 'rollback',
        attempts: failedTask.attempts,
        message: `回滚失败: ${error instanceof Error ? error.message : String(error)}`,
        rolledBack: false,
      };
    }
  }

  /**
   * 构建修复 Prompt
   */
  private buildFixPrompt(
    task: TaskState,
    errorDetails: string,
    level: 'simple' | 'detailed'
  ): string {
    if (level === 'simple') {
      return `
之前的任务执行失败，请修复以下错误：

## 原始任务
${task.description}

## 错误信息
${errorDetails}

请分析错误原因并修复代码。
`.trim();
    }

    // detailed level - 提供更多上下文
    return `
之前的任务执行失败，这是第 ${task.attempts + 1} 次尝试修复。

## 原始任务
${task.description}

## 修改的文件
${task.modifiedFiles?.join('\n') || '未知'}

## 错误信息
${errorDetails}

## 修复建议
1. 仔细分析错误信息，找出根本原因
2. 检查是否有类型错误、语法错误或逻辑错误
3. 确保修改不会引入新的问题
4. 如果需要，可以采用不同的实现方式

请修复代码并确保通过编译检查。
`.trim();
  }

  /**
   * 构建升级到 Claude 的 Prompt
   */
  private buildEscalatePrompt(task: TaskState, errorDetails: string): string {
    return `
一个任务在多次尝试后仍然失败，需要你的帮助来分析和修复。

## 任务信息
- 描述: ${task.description}
- 原执行者: ${task.assignedCli}
- 尝试次数: ${task.attempts}
- 修改的文件: ${task.modifiedFiles?.join(', ') || '未知'}

## 错误信息
${errorDetails}

## 请求
1. 分析为什么之前的修复尝试失败
2. 找出问题的根本原因
3. 提供一个可靠的修复方案
4. 确保修复后代码能通过编译和验证

请仔细分析并修复这个问题。
`.trim();
  }

  /**
   * 检查是否应该继续恢复
   */
  shouldContinueRecovery(task: TaskState): boolean {
    return task.attempts < this.config.maxAttempts;
  }

  /**
   * 获取恢复统计
   */
  getRecoveryStats(tasks: TaskState[]): {
    totalRecoveries: number;
    successfulRecoveries: number;
    failedRecoveries: number;
    rollbacks: number;
  } {
    const recoveredTasks = tasks.filter(t => t.attempts > 0);
    return {
      totalRecoveries: recoveredTasks.length,
      successfulRecoveries: recoveredTasks.filter(t => t.status === 'completed').length,
      failedRecoveries: recoveredTasks.filter(t => t.status === 'failed').length,
      rollbacks: recoveredTasks.filter(t => t.status === 'cancelled').length,
    };
  }
}

