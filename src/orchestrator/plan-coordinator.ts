/**
 * Plan Coordinator - 计划协调器
 *
 * 职责：
 * - 创建执行计划
 * - 执行已有计划
 * - 获取计划记录
 */

import { logger, LogCategory } from '../logging';
import { globalEventBus } from '../events';
import { MissionDrivenEngine } from './core';
import { PlanRecord } from './plan-storage';
import { TaskContextManager } from './task-context-manager';

/**
 * 计划协调器
 */
export class PlanCoordinator {
  private isRunning = false;
  private currentTaskId: string | null = null;
  private abortController: AbortController | null = null;

  constructor(
    private missionDrivenEngine: MissionDrivenEngine,
    private taskContextManager: TaskContextManager
  ) {}

  /**
   * 设置运行状态
   */
  setRunning(running: boolean): void {
    this.isRunning = running;
  }

  /**
   * 设置当前任务 ID
   */
  setCurrentTaskId(taskId: string | null): void {
    this.currentTaskId = taskId;
  }

  /**
   * 设置中止控制器
   */
  setAbortController(controller: AbortController | null): void {
    this.abortController = controller;
  }

  /**
   * 获取运行状态
   */
  getRunning(): boolean {
    return this.isRunning;
  }

  /**
   * 仅生成执行计划（不执行）
   */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;

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

    try {
      const record = await this.missionDrivenEngine.createPlan(userPrompt, taskId, sessionId);
      const taskManager = await this.taskContextManager.getTaskManager(sessionId);
      await taskManager.updateTaskPlan(taskId, {
        planId: record.id,
        planSummary: record.plan.summary || record.plan.analysis || '执行计划',
        status: 'ready',
      });
      await this.taskContextManager.updateTaskStatus(taskId, 'pending', sessionId);
      return record;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      await this.taskContextManager.updateTaskStatus(taskId, 'failed', sessionId, errorMsg);
      globalEventBus.emitEvent('task:failed', { taskId, data: { error: errorMsg, isRunning: false } });
      throw error;
    } finally {
      this.isRunning = false;
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  /**
   * 使用已生成的计划执行
   */
  async executePlan(record: PlanRecord, taskId?: string, sessionId?: string): Promise<string> {
    if (this.isRunning) {
      throw new Error('编排器正在运行中');
    }

    this.isRunning = true;
    const finalTaskId = taskId || record.taskId;
    await this.taskContextManager.ensureTaskExists(finalTaskId, record.prompt || '', sessionId || record.sessionId);
    this.currentTaskId = finalTaskId;
    this.abortController = new AbortController();

    await this.taskContextManager.updateTaskStatus(finalTaskId, 'running', sessionId || record.sessionId);
    globalEventBus.emitEvent('task:started', { taskId: finalTaskId, data: { isRunning: true } });

    try {
      const taskManager = await this.taskContextManager.getTaskManager(sessionId || record.sessionId);
      await taskManager.updateTaskPlan(finalTaskId, {
        planId: record.id,
        planSummary: record.plan.summary || record.plan.analysis || '执行计划',
        status: 'executing',
      });

      const result = await this.missionDrivenEngine.executePlan(
        record.plan,
        finalTaskId,
        sessionId || record.sessionId,
        record.prompt
      );

      const execStatus = this.missionDrivenEngine.getLastExecutionStatus();

      if (this.abortController?.signal.aborted) {
        await this.taskContextManager.updateTaskStatus(finalTaskId, 'cancelled', sessionId || record.sessionId);
        globalEventBus.emitEvent('task:cancelled', { taskId: finalTaskId, data: { isRunning: false } });
        return '任务已被取消。';
      }

      if (execStatus.success) {
        await taskManager.updateTaskPlanStatus(finalTaskId, 'completed');
        await this.taskContextManager.updateTaskStatus(finalTaskId, 'completed', sessionId || record.sessionId);
        globalEventBus.emitEvent('task:completed', { taskId: finalTaskId, data: { isRunning: false } });
      } else {
        const errorMsg = execStatus.errors.join('; ') || '任务执行失败';
        await taskManager.updateTaskPlanStatus(finalTaskId, 'failed');
        await this.taskContextManager.updateTaskStatus(finalTaskId, 'failed', sessionId || record.sessionId, errorMsg);
        globalEventBus.emitEvent('task:failed', { taskId: finalTaskId, data: { error: errorMsg, isRunning: false } });
      }

      return result;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      const taskManager = await this.taskContextManager.getTaskManager(sessionId || record.sessionId);
      await taskManager.updateTaskPlanStatus(finalTaskId, 'failed');
      await this.taskContextManager.updateTaskStatus(finalTaskId, 'failed', sessionId || record.sessionId, errorMsg);
      globalEventBus.emitEvent('task:failed', { taskId: finalTaskId, data: { error: errorMsg, isRunning: false } });
      throw error;
    } finally {
      this.isRunning = false;
      this.abortController = null;
      this.currentTaskId = null;
    }
  }

  /**
   * 获取会话的活动计划
   */
  getActivePlanForSession(sessionId: string): PlanRecord | null {
    return this.missionDrivenEngine.getActivePlanForSession(sessionId);
  }

  /**
   * 获取会话的最新计划
   */
  getLatestPlanForSession(sessionId: string): PlanRecord | null {
    return this.missionDrivenEngine.getLatestPlanForSession(sessionId);
  }

  /**
   * 根据 ID 获取计划
   */
  getPlanById(planId: string, sessionId: string): PlanRecord | null {
    return this.missionDrivenEngine.getPlanById(planId, sessionId);
  }
}
