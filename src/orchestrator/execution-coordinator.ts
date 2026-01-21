/**
 * Execution Coordinator - 执行协调器
 *
 * 职责：
 * - 协调任务执行流程
 * - 管理执行状态
 * - 处理执行结果
 * - 错误处理和回滚
 */

import { logger, LogCategory } from '../logging';
import { globalEventBus } from '../events';
import { IAdapterFactory } from '../adapters/adapter-factory-interface';
import { MissionDrivenEngine } from './core';
import { SnapshotManager } from '../snapshot-manager';
import { TaskContextManager } from './task-context-manager';
import { InteractionModeManager } from './interaction-mode-manager';
import { UnifiedSessionManager } from '../session';

/**
 * 执行协调器
 */
export class ExecutionCoordinator {
  private isRunning = false;
  private currentTaskId: string | null = null;
  private abortController: AbortController | null = null;
  private statusUpdateInterval: NodeJS.Timeout | null = null;

  constructor(
    private adapterFactory: IAdapterFactory,
    private missionDrivenEngine: MissionDrivenEngine,
    private snapshotManager: SnapshotManager,
    private taskContextManager: TaskContextManager,
    private interactionModeManager: InteractionModeManager,
    private sessionManager: UnifiedSessionManager,
    private autoRollbackOnFailure: boolean
  ) {}

  /**
   * 是否正在运行
   */
  get running(): boolean {
    return this.isRunning;
  }

  /**
   * 获取当前任务 ID
   */
  getCurrentTaskId(): string | null {
    return this.currentTaskId;
  }

  /**
   * 执行任务 - 主入口
   */
  async execute(userPrompt: string, taskId?: string, sessionId?: string): Promise<string> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;
    const shouldAsk = this.interactionModeManager.shouldUseAskMode(userPrompt)
      || this.interactionModeManager.getInteractionMode() === 'ask';

    if (shouldAsk) {
      try {
        return await this.executeAskMode(userPrompt, taskId, sessionId);
      } finally {
        this.isRunning = false;
        this.stopStatusUpdates();
        this.abortController = null;
        this.currentTaskId = null;
      }
    }

    if (!taskId) {
      const taskManager = await this.taskContextManager.getTaskManager(sessionId);
      const task = await taskManager.createTask({ prompt: userPrompt });
      taskId = task.id;
    } else {
      await this.taskContextManager.ensureTaskExists(taskId, userPrompt, sessionId);
    }

    this.currentTaskId = taskId;
    this.abortController = new AbortController();

    await this.taskContextManager.updateTaskStatus(taskId, 'running', sessionId);
    globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
    this.startStatusUpdates(taskId);

    try {
      // agent/auto 模式：使用 MissionDrivenEngine 执行
      const result = await this.missionDrivenEngine.execute(userPrompt, taskId, sessionId);
      const execStatus = this.missionDrivenEngine.getLastExecutionStatus();

      if (this.abortController?.signal.aborted) {
        await this.taskContextManager.updateTaskStatus(taskId, 'cancelled', sessionId);
        globalEventBus.emitEvent('task:cancelled', { taskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      if (execStatus.success) {
        await this.taskContextManager.updateTaskStatus(taskId, 'completed', sessionId);
        globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });
      } else {
        const errorMsg = execStatus.errors.join('; ') || '任务执行失败';
        await this.taskContextManager.updateTaskStatus(taskId, 'failed', sessionId, errorMsg);
        globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      }

      return result;

    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);

      if (this.abortController?.signal.aborted) {
        await this.taskContextManager.updateTaskStatus(taskId, 'cancelled', sessionId);
        globalEventBus.emitEvent('task:cancelled', { taskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      const modeConfig = this.interactionModeManager.getModeConfig();
      if (modeConfig.autoRollbackOnFailure && this.autoRollbackOnFailure) {
        const count = this.snapshotManager.revertAllChanges();
        logger.info('编排器.回滚.自动.完成', { count }, LogCategory.ORCHESTRATOR);
      }

      await this.taskContextManager.updateTaskStatus(taskId, 'failed', sessionId, errorMsg);
      globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      throw error;

    } finally {
      this.isRunning = false;
      this.stopStatusUpdates();
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  /**
   * 执行任务并返回任务上下文
   */
  async executeWithTaskContext(userPrompt: string, sessionId?: string): Promise<{ taskId: string; result: string }> {
    const shouldAsk = this.interactionModeManager.shouldUseAskMode(userPrompt)
      || this.interactionModeManager.getInteractionMode() === 'ask';

    if (shouldAsk) {
      const result = await this.execute(userPrompt, undefined, sessionId);
      return { taskId: '', result };
    }

    const taskManager = await this.taskContextManager.getTaskManager(sessionId);
    const task = await taskManager.createTask({ prompt: userPrompt });
    const taskId = task.id;
    const result = await this.execute(userPrompt, taskId, sessionId);
    return { taskId, result };
  }

  /**
   * ask 模式：仅对话
   */
  private async executeAskMode(userPrompt: string, taskId?: string, sessionId?: string): Promise<string> {
    logger.info('编排器.执行.对话_模式', undefined, LogCategory.ORCHESTRATOR);

    const contextSessionId = sessionId || taskId || this.sessionManager.getCurrentSession()?.id || '';
    const taskManager = contextSessionId ? await this.taskContextManager.getTaskManager(contextSessionId) : null;
    const task = taskManager && taskId ? await taskManager.getTask(taskId) : null;

    if (taskId) {
      if (task) {
        await this.taskContextManager.updateTaskStatus(taskId, 'running', sessionId);
      }
      globalEventBus.emitEvent('task:started', { taskId, data: { isRunning: true } });
      this.startStatusUpdates(taskId);
    }

    const context = await this.missionDrivenEngine.prepareContext(contextSessionId, userPrompt);
    const prompt = context
      ? `请结合以下会话上下文回答用户问题。\n\n${context}\n\n## 用户问题\n${userPrompt}`
      : userPrompt;
    const snapshot = context ? this.truncateSnapshot(context) : undefined;

    const response = await this.adapterFactory.sendMessage(
      'claude',
      prompt,
      undefined,
      {
        source: 'orchestrator',
        streamToUI: true,
        adapterRole: 'orchestrator',
        messageMeta: {
          taskId: taskId || '',
          intent: 'ask',
          contextSnapshot: snapshot,
        },
      }
    );

    this.missionDrivenEngine.recordOrchestratorTokens(response.tokenUsage);

    if (response.error) {
      if (taskId) {
        if (task) {
          await this.taskContextManager.updateTaskStatus(taskId, 'failed', sessionId, response.error);
        }
        globalEventBus.emitEvent('task:failed', { taskId, data: { error: response.error, isRunning: false } });
      }
      throw new Error(response.error);
    }

    if (taskId) {
      if (task) {
        await this.taskContextManager.updateTaskStatus(taskId, 'completed', sessionId);
      }
      globalEventBus.emitEvent('task:completed', { taskId, data: { isRunning: false } });
    }

    const content = response.content || '';
    await this.missionDrivenEngine.recordAssistantMessage(content);
    return content;
  }

  /**
   * 截断快照
   */
  private truncateSnapshot(context: string, maxChars: number = 6000): string {
    const trimmed = context.trim();
    if (trimmed.length <= maxChars) return trimmed;
    return trimmed.slice(0, maxChars) + '\n...';
  }

  /**
   * 取消当前任务
   */
  async cancel(): Promise<void> {
    logger.info('编排器.任务.取消.请求', undefined, LogCategory.ORCHESTRATOR);

    // 1. 触发 AbortController
    this.abortController?.abort();

    // 2. 取消 MissionDrivenEngine 中的任务
    await this.missionDrivenEngine.cancel();

    // 3. 停止状态更新定时器
    this.stopStatusUpdates();

    if (this.currentTaskId) {
      await this.taskContextManager.updateTaskStatus(this.currentTaskId, 'cancelled');
      globalEventBus.emitEvent('task:cancelled', { taskId: this.currentTaskId, data: { isRunning: false } });
    }

    // 4. 重置状态标志
    this.isRunning = false;
    this.abortController = null;
    this.currentTaskId = null;

    logger.info('编排器.任务.取消.完成', undefined, LogCategory.ORCHESTRATOR);
  }

  /**
   * 中断当前任务
   */
  async interrupt(): Promise<void> {
    await this.cancel();
  }

  /**
   * 开始状态更新定时器
   */
  private startStatusUpdates(taskId: string): void {
    this.stopStatusUpdates();
    this.statusUpdateInterval = setInterval(() => {
      if (this.isRunning) {
        globalEventBus.emitEvent('orchestrator:phase_changed', {
          taskId,
          data: { phase: this.missionDrivenEngine.state, isRunning: true },
        });
      }
    }, 2000);
  }

  /**
   * 停止状态更新定时器
   */
  private stopStatusUpdates(): void {
    if (this.statusUpdateInterval) {
      clearInterval(this.statusUpdateInterval);
      this.statusUpdateInterval = null;
    }
  }
}
