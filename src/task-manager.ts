/**
 * Task 管理器
 * 管理 Task 创建、状态更新、SubTask 分解
 *
 * @deprecated 自 v0.8.0 起废弃，请使用 UnifiedTaskManager
 * @deprecated-since v0.8.0
 * @deprecated-reason 功能已完全迁移到 UnifiedTaskManager，该类将在下一个大版本中删除
 *
 * 迁移指南:
 * - 使用 UnifiedTaskManager 替代 TaskManager
 * - createTask() → unifiedTaskManager.createTask()
 * - updateTaskStatus() → unifiedTaskManager.startTask() / completeTask() / failTask()
 * - addSubTask() → unifiedTaskManager.createSubTask()
 * - updateSubTaskStatus() → unifiedTaskManager.startSubTask() / completeSubTask() / failSubTask()
 * - updateTaskPlan() → unifiedTaskManager.updateTaskPlan()
 * - updateTaskPlanStatus() → unifiedTaskManager.updateTaskPlanStatus()
 * - addExistingSubTask() → unifiedTaskManager.addExistingSubTask()
 * - updateSubTaskFiles() → unifiedTaskManager.updateSubTaskFiles()
 *
 * @see UnifiedTaskManager
 */

import { Task, SubTask, TaskStatus, SubTaskStatus, CLIType, WorkerType } from './types';
import { UnifiedSessionManager } from './session';
import { globalEventBus } from './events';

/** 生成唯一 ID */
function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * Task 管理器
 *
 * @deprecated 自 v0.8.0 起废弃，请使用 UnifiedTaskManager
 */
export class TaskManager {
  private sessionManager: UnifiedSessionManager;

  constructor(sessionManager: UnifiedSessionManager) {
    this.sessionManager = sessionManager;
  }

  /** 创建新 Task */
  createTask(prompt: string): Task {
    // 输入验证：确保 prompt 有效
    if (!prompt || typeof prompt !== 'string') {
      throw new Error('Prompt must be a non-empty string');
    }

    const trimmedPrompt = prompt.trim();
    if (trimmedPrompt.length === 0) {
      throw new Error('Prompt cannot be empty');
    }

    if (trimmedPrompt.length > 50000) {
      throw new Error('Prompt too long (maximum 50000 characters)');
    }

    const session = this.sessionManager.getOrCreateCurrentSession();

    const task: Task = {
      id: generateId(),
      sessionId: session.id,
      prompt: trimmedPrompt,
      status: 'pending',
      priority: 5,
      subTasks: [],
      createdAt: Date.now(),
      retryCount: 0,
      maxRetries: 3,
    };

    this.sessionManager.addTask(session.id, task);
    globalEventBus.emitEvent('task:created', { sessionId: session.id, taskId: task.id });

    return task;
  }

  /** 获取 Task */
  getTask(taskId: string): Task | null {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return null;
    return session.tasks.find(t => t.id === taskId) ?? null;
  }

  /** 更新 Task 内容（非状态字段） */
  updateTask(taskId: string, updates: Partial<Task>): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return;
    const task = session.tasks.find(t => t.id === taskId);
    if (!task) return;
    const next = { ...task, ...updates };
    this.sessionManager.updateTask(session.id, taskId, next);
  }

  /** 更新 Task 关联的执行计划信息 */
  updateTaskPlan(taskId: string, planInfo: { planId: string; planSummary?: string; status?: Task['planStatus'] }): void {
    const updates: Partial<Task> = {
      planId: planInfo.planId,
      planSummary: planInfo.planSummary,
      planStatus: planInfo.status ?? 'ready',
      planCreatedAt: Date.now(),
      planUpdatedAt: Date.now(),
    };
    this.updateTask(taskId, updates);
  }

  /** 更新 Task 的执行计划状态 */
  updateTaskPlanStatus(taskId: string, status: Task['planStatus']): void {
    this.updateTask(taskId, {
      planStatus: status,
      planUpdatedAt: Date.now(),
    });
  }

  /** 更新 Task 状态 */
  updateTaskStatus(taskId: string, status: TaskStatus): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return;

    const task = session.tasks.find(t => t.id === taskId);
    if (!task) return;

    task.status = status;

    // 更新时间戳
    if (status === 'running' && !task.startedAt) {
      task.startedAt = Date.now();
    } else if (status === 'completed' || status === 'failed') {
      task.completedAt = Date.now();
    } else if (status === 'cancelled') {
      task.cancelledAt = Date.now();
    }

    this.sessionManager.updateTask(session.id, taskId, task);

    // 发布事件
    const eventType = status === 'completed' ? 'task:completed'
      : status === 'failed' ? 'task:failed'
      : status === 'cancelled' ? 'task:cancelled'
      : 'task:started';

    globalEventBus.emitEvent(eventType, { sessionId: session.id, taskId });
  }

  /** 添加 SubTask（使用统一类型） */
  addSubTask(
    taskId: string,
    description: string,
    assignedWorker: WorkerType,
    targetFiles: string[] = [],
    options?: {
      reason?: string;
      prompt?: string;
      dependencies?: string[];
      priority?: number;
    }
  ): SubTask {
    const session = this.sessionManager.getCurrentSession();
    if (!session) throw new Error('没有活动的 Session');

    const task = session.tasks.find(t => t.id === taskId);
    if (!task) throw new Error(`Task 不存在: ${taskId}`);

    const subTask: SubTask = {
      id: generateId(),
      taskId,
      description,
      assignedWorker,
      reason: options?.reason,
      prompt: options?.prompt,
      targetFiles,
      dependencies: options?.dependencies || [],
      priority: options?.priority ?? 5,
      status: 'pending',
      progress: 0,
      retryCount: 0,
      maxRetries: 3,
      output: [],
    };

    task.subTasks.push(subTask);
    this.sessionManager.updateTask(session.id, taskId, task);

    return subTask;
  }

  /** 注册既有 SubTask（用于编排计划落库） */
  addExistingSubTask(taskId: string, subTask: SubTask): SubTask {
    const session = this.sessionManager.getCurrentSession();
    if (!session) throw new Error('没有活动的 Session');

    const task = session.tasks.find(t => t.id === taskId);
    if (!task) throw new Error(`Task 不存在: ${taskId}`);

    const existing = task.subTasks.find(st => st.id === subTask.id);
    if (existing) {
      return existing;
    }

    const normalized: SubTask = {
      ...subTask,
      taskId,
      targetFiles: subTask.targetFiles ?? [],
      modifiedFiles: subTask.modifiedFiles ?? [],
      dependencies: subTask.dependencies ?? [],
      status: subTask.status ?? 'pending',
      output: subTask.output ?? [],
      progress: subTask.progress ?? 0,
      retryCount: subTask.retryCount ?? 0,
      maxRetries: subTask.maxRetries ?? 3,
      priority: subTask.priority ?? 5,
    };

    task.subTasks.push(normalized);
    this.sessionManager.updateTask(session.id, taskId, task);
    return normalized;
  }

  /** 更新 SubTask 状态 */
  updateSubTaskStatus(taskId: string, subTaskId: string, status: SubTaskStatus): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return;

    const task = session.tasks.find(t => t.id === taskId);
    if (!task) return;

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) return;

    subTask.status = status;

    if (task.status === 'pending' && status !== 'pending') {
      task.status = 'running';
      if (!task.startedAt) {
        task.startedAt = Date.now();
      }
    }
    
    if (status === 'running' && !subTask.startedAt) {
      subTask.startedAt = Date.now();
    } else if (status === 'completed' || status === 'failed') {
      subTask.completedAt = Date.now();
    }

    this.sessionManager.updateTask(session.id, taskId, task);

    // 检查是否所有 SubTask 都完成了
    this.checkTaskCompletion(taskId);
  }

  /** 更新 SubTask 的实际修改文件 */
  updateSubTaskFiles(taskId: string, subTaskId: string, files: string[]): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return;

    const task = session.tasks.find(t => t.id === taskId);
    if (!task) return;

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) return;

    const normalized = Array.from(
      new Set((files || []).filter(f => typeof f === 'string' && f.trim()))
    );
    subTask.modifiedFiles = normalized;

    this.sessionManager.updateTask(session.id, taskId, task);
  }

  /** 添加 SubTask 输出 */
  addSubTaskOutput(taskId: string, subTaskId: string, output: string): void {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return;

    const task = session.tasks.find(t => t.id === taskId);
    if (!task) return;

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) return;

    subTask.output.push(output);
    this.sessionManager.updateTask(session.id, taskId, task);
  }

  /** 检查 Task 是否完成 */
  private checkTaskCompletion(taskId: string): void {
    const task = this.getTask(taskId);
    if (!task || task.status !== 'running') return;

    const allCompleted = task.subTasks.every(st => 
      st.status === 'completed' || st.status === 'skipped'
    );
    const anyFailed = task.subTasks.some(st => st.status === 'failed');

    if (anyFailed) {
      this.updateTaskStatus(taskId, 'failed');
    } else if (allCompleted) {
      this.updateTaskStatus(taskId, 'completed');
    }
  }

  /** 取消 Task */
  cancelTask(taskId: string): void {
    this.updateTaskStatus(taskId, 'cancelled');
  }

  /** 获取当前 Session 的所有 Task */
  getAllTasks(): Task[] {
    const session = this.sessionManager.getCurrentSession();
    return session?.tasks ?? [];
  }
}
