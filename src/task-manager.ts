/**
 * Task 管理器
 * 管理 Task 创建、状态更新、SubTask 分解
 */

import { Task, SubTask, TaskStatus, SubTaskStatus, CLIType, WorkerType } from './types';
import { SessionManager } from './session-manager';
import { globalEventBus } from './events';

/** 生成唯一 ID */
function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * Task 管理器
 */
export class TaskManager {
  private sessionManager: SessionManager;

  constructor(sessionManager: SessionManager) {
    this.sessionManager = sessionManager;
  }

  /** 创建新 Task */
  createTask(prompt: string): Task {
    const session = this.sessionManager.getOrCreateCurrentSession();
    
    const task: Task = {
      id: generateId(),
      sessionId: session.id,
      prompt,
      status: 'pending',
      subTasks: [],
      createdAt: Date.now(),
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
    } else if (status === 'interrupted') {
      task.interruptedAt = Date.now();
    }

    this.sessionManager.updateTask(session.id, taskId, task);

    // 发布事件
    const eventType = status === 'completed' ? 'task:completed' 
      : status === 'failed' ? 'task:failed' 
      : status === 'interrupted' ? 'task:interrupted'
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
      priority: options?.priority,
      status: 'pending',
      output: [],
    };

    task.subTasks.push(subTask);
    this.sessionManager.updateTask(session.id, taskId, task);

    return subTask;
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
    
    if (status === 'running' && !subTask.startedAt) {
      subTask.startedAt = Date.now();
    } else if (status === 'completed' || status === 'failed') {
      subTask.completedAt = Date.now();
    }

    this.sessionManager.updateTask(session.id, taskId, task);

    // 检查是否所有 SubTask 都完成了
    this.checkTaskCompletion(taskId);
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

  /** 打断 Task */
  interruptTask(taskId: string): void {
    this.updateTaskStatus(taskId, 'interrupted');
  }

  /** 获取当前 Session 的所有 Task */
  getAllTasks(): Task[] {
    const session = this.sessionManager.getCurrentSession();
    return session?.tasks ?? [];
  }
}

