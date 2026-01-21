/**
 * Task Context Manager - 任务上下文管理器
 *
 * 职责：
 * - 管理 TaskManager 实例
 * - 解析会话 ID
 * - 确保任务存在
 * - 更新任务状态
 */

import { TaskStatus } from '../types';
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
import { UnifiedSessionManager } from '../session';

/**
 * 任务上下文管理器
 */
export class TaskContextManager {
  private taskManager: UnifiedTaskManager | null = null;
  private taskManagerSessionId: string | null = null;

  constructor(private sessionManager: UnifiedSessionManager) {}

  /**
   * 设置任务管理器
   */
  setTaskManager(taskManager: UnifiedTaskManager, sessionId: string): void {
    this.taskManager = taskManager;
    this.taskManagerSessionId = sessionId;
  }

  /**
   * 获取任务管理器
   */
  async getTaskManager(sessionId?: string): Promise<UnifiedTaskManager> {
    const resolvedSessionId = this.resolveSessionId(sessionId);

    // 如果已有相同会话的 TaskManager，直接返回
    if (this.taskManager && this.taskManagerSessionId === resolvedSessionId) {
      return this.taskManager;
    }

    // 创建新的 TaskManager
    const repository = new SessionManagerTaskRepository(this.sessionManager, resolvedSessionId);
    const manager = new UnifiedTaskManager(resolvedSessionId, repository);
    await manager.initialize();

    this.setTaskManager(manager, resolvedSessionId);
    return manager;
  }

  /**
   * 解析会话 ID
   */
  resolveSessionId(sessionId?: string): string {
    const resolved = sessionId || this.sessionManager.getCurrentSession()?.id || '';
    if (!resolved) {
      throw new Error('未找到有效的会话 ID');
    }
    return resolved;
  }

  /**
   * 确保任务存在
   */
  async ensureTaskExists(taskId: string, prompt: string, sessionId?: string): Promise<void> {
    const taskManager = await this.getTaskManager(sessionId);
    const existing = await taskManager.getTask(taskId);
    if (existing) {
      return;
    }
    await taskManager.createTask({ id: taskId, prompt });
  }

  /**
   * 更新任务状态
   */
  async updateTaskStatus(
    taskId: string,
    status: TaskStatus,
    sessionId?: string,
    error?: string
  ): Promise<void> {
    const taskManager = await this.getTaskManager(sessionId);
    const task = await taskManager.getTask(taskId);
    if (!task) {
      return;
    }

    if (status === 'running') {
      if (task.status !== 'running') {
        await taskManager.startTask(taskId);
      }
      return;
    }

    if (status === 'completed') {
      await taskManager.completeTask(taskId);
      return;
    }

    if (status === 'failed') {
      await taskManager.failTask(taskId, error);
      return;
    }

    if (status === 'cancelled') {
      await taskManager.cancelTask(taskId);
      return;
    }

    if (status === 'pending') {
      if (task.status !== 'pending') {
        await taskManager.updateTask(taskId, { status: 'pending' });
      }
      return;
    }

    await taskManager.updateTask(taskId, { status });
  }

  /**
   * 获取当前 TaskManager（如果存在）
   */
  getCurrentTaskManager(): UnifiedTaskManager | null {
    return this.taskManager;
  }

  /**
   * 获取当前会话 ID（如果存在）
   */
  getCurrentSessionId(): string | null {
    return this.taskManagerSessionId;
  }
}
