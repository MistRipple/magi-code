/**
 * 失败恢复处理器
 * 基于失败类型的恢复治理，负责 Phase 5 的失败恢复
 */

import { logger, LogCategory } from '../logging';
import { CLIType } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { SnapshotManager } from '../snapshot-manager';
import { globalEventBus } from '../events';
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SubTask } from '../task/types';
import { VerificationResult } from './verification-runner';

/** 恢复策略 */
export type RecoveryStrategy =
  | 'retry_same_cli'      // 原 CLI 修复
  | 'retry_with_context'  // 提供更多上下文
  | 'escalate_to_claude'  // 升级到 Claude
  | 'rollback';           // 回滚

/** 失败类型 */
export type FailureType =
  | 'tool_failure'
  | 'compile_failure'
  | 'test_failure'
  | 'logic_failure'
  | 'dependency_failure'
  | 'unknown';

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
  private unifiedTaskManager: UnifiedTaskManager;
  private config: RecoveryConfig;

  constructor(
    cliFactory: CLIAdapterFactory,
    snapshotManager: SnapshotManager,
    unifiedTaskManager: UnifiedTaskManager,
    config?: Partial<RecoveryConfig>
  ) {
    this.cliFactory = cliFactory;
    this.snapshotManager = snapshotManager;
    this.unifiedTaskManager = unifiedTaskManager;
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 执行恢复流程
   */
  async recover(
    taskId: string,
    failedTask: SubTask,
    verificationResult: VerificationResult,
    errorDetails: string
  ): Promise<RecoveryResult> {
    const attempts = failedTask.retryCount;
    logger.info('编排器.恢复.开始', { attempts }, LogCategory.ORCHESTRATOR);

    // 确定恢复策略
    const failureType = this.classifyFailure(errorDetails, verificationResult);

    globalEventBus.emitEvent('recovery:started', {
      taskId,
      data: { attempts, maxAttempts: this.config.maxAttempts, failureType }
    });
    const strategy = this.determineStrategy(attempts, failureType);
    logger.info('编排器.恢复.策略_选择', { failureType, strategy }, LogCategory.ORCHESTRATOR);

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
  private determineStrategy(attempts: number, failureType: FailureType): RecoveryStrategy {
    if (attempts >= this.config.maxAttempts) {
      return 'rollback';
    }
    const plan = this.getRecoveryPlan(failureType);
    const index = Math.min(attempts, plan.length - 1);
    return plan[index];
  }

  private getRecoveryPlan(failureType: FailureType): RecoveryStrategy[] {
    const plans: Record<FailureType, RecoveryStrategy[]> = {
      tool_failure: ['retry_same_cli', 'retry_with_context', 'rollback'],
      compile_failure: ['retry_with_context', 'escalate_to_claude', 'rollback'],
      test_failure: ['retry_with_context', 'escalate_to_claude', 'rollback'],
      logic_failure: ['escalate_to_claude', 'rollback'],
      dependency_failure: ['escalate_to_claude', 'rollback'],
      unknown: ['retry_with_context', 'escalate_to_claude', 'rollback'],
    };
    return plans[failureType] ?? plans.unknown;
  }

  /**
   * 原 CLI 尝试修复
   */
  private async retrySameCli(
    taskId: string,
    failedTask: SubTask,
    errorDetails: string
  ): Promise<RecoveryResult> {
    logger.info('编排器.恢复.重试.原始_CLI', { cli: failedTask.assignedWorker }, LogCategory.ORCHESTRATOR);

    await this.unifiedTaskManager.resetSubTaskForRetry(taskId, failedTask.id);
    await this.unifiedTaskManager.startSubTask(taskId, failedTask.id);

    const fixPrompt = this.buildFixPrompt(failedTask, errorDetails, 'simple');

    try {
      const response = await this.cliFactory.sendMessage(failedTask.assignedWorker, fixPrompt);

      if (response.error) {
        await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, response.error);
        return {
          success: false,
          strategy: 'retry_same_cli',
          attempts: failedTask.retryCount,
          message: `修复失败: ${response.error}`,
        };
      }

      if (response.content) {
        await this.unifiedTaskManager.completeSubTask(taskId, failedTask.id, {
          cliType: failedTask.assignedWorker,
          success: true,
          output: response.content || '',
          modifiedFiles: failedTask.modifiedFiles || [],
          duration: 0,
          timestamp: new Date(),
        });
      }
      return {
        success: true,
        strategy: 'retry_same_cli',
        attempts: failedTask.retryCount,
        message: '原 CLI 修复成功',
      };
    } catch (error) {
      await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, String(error));
      return {
        success: false,
        strategy: 'retry_same_cli',
        attempts: failedTask.retryCount,
        message: `修复异常: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
  }

  /**
   * 提供更多上下文重试
   */
  private async retryWithContext(
    taskId: string,
    failedTask: SubTask,
    errorDetails: string
  ): Promise<RecoveryResult> {
    logger.info('编排器.恢复.提供_上下文', { cli: failedTask.assignedWorker }, LogCategory.ORCHESTRATOR);

    await this.unifiedTaskManager.resetSubTaskForRetry(taskId, failedTask.id);
    await this.unifiedTaskManager.startSubTask(taskId, failedTask.id);

    const fixPrompt = this.buildFixPrompt(failedTask, errorDetails, 'detailed');

    try {
      const response = await this.cliFactory.sendMessage(failedTask.assignedWorker, fixPrompt);

      if (response.error) {
        await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, response.error);
        return {
          success: false,
          strategy: 'retry_with_context',
          attempts: failedTask.retryCount,
          message: `带上下文修复失败: ${response.error}`,
        };
      }

      if (response.content) {
        await this.unifiedTaskManager.completeSubTask(taskId, failedTask.id, {
          cliType: failedTask.assignedWorker,
          success: true,
          output: response.content || '',
          modifiedFiles: failedTask.modifiedFiles || [],
          duration: 0,
          timestamp: new Date(),
        });
      }
      return {
        success: true,
        strategy: 'retry_with_context',
        attempts: failedTask.retryCount,
        message: '带上下文修复成功',
      };
    } catch (error) {
      await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, String(error));
      return {
        success: false,
        strategy: 'retry_with_context',
        attempts: failedTask.retryCount,
        message: `修复异常: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
  }

  /**
   * 升级到 Claude 处理
   */
  private async escalateToClaude(
    taskId: string,
    failedTask: SubTask,
    errorDetails: string
  ): Promise<RecoveryResult> {
    logger.info('编排器.恢复.升级', { cli: this.config.escalateCli }, LogCategory.ORCHESTRATOR);

    await this.unifiedTaskManager.resetSubTaskForRetry(taskId, failedTask.id);
    await this.unifiedTaskManager.startSubTask(taskId, failedTask.id);

    const escalatePrompt = this.buildEscalatePrompt(failedTask, errorDetails);

    try {
      const response = await this.cliFactory.sendMessage(this.config.escalateCli, escalatePrompt);

      if (response.error) {
        await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, response.error);
        return {
          success: false,
          strategy: 'escalate_to_claude',
          attempts: failedTask.retryCount,
          message: `Claude 修复失败: ${response.error}`,
        };
      }

      if (response.content) {
        await this.unifiedTaskManager.completeSubTask(taskId, failedTask.id, {
          cliType: failedTask.assignedWorker,
          success: true,
          output: response.content || '',
          modifiedFiles: failedTask.modifiedFiles || [],
          duration: 0,
          timestamp: new Date(),
        });
      }
      return {
        success: true,
        strategy: 'escalate_to_claude',
        attempts: failedTask.retryCount,
        message: 'Claude 修复成功',
      };
    } catch (error) {
      await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, String(error));
      return {
        success: false,
        strategy: 'escalate_to_claude',
        attempts: failedTask.retryCount,
        message: `Claude 修复异常: ${error instanceof Error ? error.message : String(error)}`,
      };
    }
  }

  /**
   * 执行回滚
   */
  private async performRollback(
    taskId: string,
    failedTask: SubTask
  ): Promise<RecoveryResult> {
    logger.info('编排器.恢复.回滚', undefined, LogCategory.ORCHESTRATOR);

    if (!this.config.enableRollback) {
      return {
        success: false,
        strategy: 'rollback',
        attempts: failedTask.retryCount,
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

      await this.unifiedTaskManager.skipSubTask(taskId, failedTask.id);

      return {
        success: false,
        strategy: 'rollback',
        attempts: failedTask.retryCount,
        message: `任务失败，已回滚 ${modifiedFiles.length} 个文件`,
        rolledBack: true,
      };
    } catch (error) {
      await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, String(error));
      return {
        success: false,
        strategy: 'rollback',
        attempts: failedTask.retryCount,
        message: `回滚失败: ${error instanceof Error ? error.message : String(error)}`,
        rolledBack: false,
      };
    }
  }

  private classifyFailure(errorDetails: string, verificationResult: VerificationResult): FailureType {
    const text = `${errorDetails} ${verificationResult?.summary || ''}`.toLowerCase();
    if (text.includes('timeout') || text.includes('超时') || text.includes('process')) {
      return 'tool_failure';
    }
    if (text.includes('compile') || text.includes('tsc') || text.includes('build') || text.includes('编译')) {
      return 'compile_failure';
    }
    if (text.includes('test') || text.includes('测试') || text.includes('jest') || text.includes('vitest')) {
      return 'test_failure';
    }
    if (text.includes('dependency') || text.includes('依赖') || text.includes('module not found')) {
      return 'dependency_failure';
    }
    if (text.includes('logic') || text.includes('逻辑') || text.includes('contract') || text.includes('契约')) {
      return 'logic_failure';
    }
    return 'unknown';
  }

  /**
   * 构建修复 Prompt
   */
  private buildFixPrompt(
    task: SubTask,
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
之前的任务执行失败，这是第 ${task.retryCount + 1} 次尝试修复。

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
  private buildEscalatePrompt(task: SubTask, errorDetails: string): string {
    return `
一个任务在多次尝试后仍然失败，需要你的帮助来分析和修复。

## 任务信息
- 描述: ${task.description}
- 原执行者: ${task.assignedWorker}
- 尝试次数: ${task.retryCount}
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
  shouldContinueRecovery(task: SubTask): boolean {
    return task.retryCount < this.config.maxAttempts;
  }

  /**
   * 获取恢复统计
   */
  getRecoveryStats(tasks: SubTask[]): {
    totalRecoveries: number;
    successfulRecoveries: number;
    failedRecoveries: number;
    rollbacks: number;
  } {
    const recoveredTasks = tasks.filter(t => t.retryCount > 0);
    return {
      totalRecoveries: recoveredTasks.length,
      successfulRecoveries: recoveredTasks.filter(t => t.status === 'completed').length,
      failedRecoveries: recoveredTasks.filter(t => t.status === 'failed').length,
      rollbacks: recoveredTasks.filter(t => t.status === 'skipped').length,
    };
  }
}
