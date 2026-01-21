/**
 * SessionManagerTaskRepository - 适配器
 *
 * 将 UnifiedSessionManager 适配为 TaskRepository 接口
 * 供 UnifiedTaskManager 通过 Session 层持久化任务数据
 */

import {
  TaskRepository,
  Transaction,
  TransactionOperation,
  TaskQuery,
  SubTaskQuery,
  RepositoryStats,
} from './task-repository';
import { Task, SubTask, TaskStatus, SubTaskStatus } from './types';
import { UnifiedSessionManager } from '../session';

export class SessionManagerTaskRepository implements TaskRepository {
  private sessionManager: UnifiedSessionManager;
  private sessionId: string;

  constructor(sessionManager: UnifiedSessionManager, sessionId: string) {
    this.sessionManager = sessionManager;
    this.sessionId = sessionId;
  }

  // ============================================================================
  // Task 操作
  // ============================================================================

  async saveTask(task: Task): Promise<void> {
    this.sessionManager.updateTask(this.sessionId, task.id, task);
  }

  async getTask(taskId: string): Promise<Task | null> {
    const session = this.sessionManager.getSession(this.sessionId);
    if (!session) return null;
    return session.tasks.find(t => t.id === taskId) || null;
  }

  async getTasksBySession(sessionId: string): Promise<Task[]> {
    const session = this.sessionManager.getSession(sessionId);
    return session?.tasks || [];
  }

  async getTasksByStatus(status: TaskStatus): Promise<Task[]> {
    const session = this.sessionManager.getSession(this.sessionId);
    if (!session) return [];
    return session.tasks.filter(t => t.status === status);
  }

  async getAllTasks(): Promise<Task[]> {
    const session = this.sessionManager.getSession(this.sessionId);
    return session?.tasks || [];
  }

  async deleteTask(taskId: string): Promise<void> {
    const session = this.sessionManager.getSession(this.sessionId);
    if (!session) return;

    const index = session.tasks.findIndex(t => t.id === taskId);
    if (index >= 0) {
      session.tasks.splice(index, 1);
      this.sessionManager.saveSession(session);
    }
  }

  // ============================================================================
  // SubTask 操作
  // ============================================================================

  async saveSubTask(taskId: string, subTask: SubTask): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const index = task.subTasks.findIndex(st => st.id === subTask.id);
    if (index >= 0) {
      task.subTasks[index] = subTask;
    } else {
      task.subTasks.push(subTask);
    }

    await this.saveTask(task);
  }

  async getSubTask(taskId: string, subTaskId: string): Promise<SubTask | null> {
    const task = await this.getTask(taskId);
    if (!task) return null;
    return task.subTasks.find(st => st.id === subTaskId) || null;
  }

  async getSubTasksByTask(taskId: string): Promise<SubTask[]> {
    const task = await this.getTask(taskId);
    return task?.subTasks || [];
  }

  async getSubTasksByStatus(status: SubTaskStatus): Promise<SubTask[]> {
    const tasks = await this.getAllTasks();
    const result: SubTask[] = [];
    for (const task of tasks) {
      result.push(...task.subTasks.filter(st => st.status === status));
    }
    return result;
  }

  // ============================================================================
  // 批量操作
  // ============================================================================

  async saveTasks(tasks: Task[]): Promise<void> {
    for (const task of tasks) {
      await this.saveTask(task);
    }
  }

  async saveSubTasks(taskId: string, subTasks: SubTask[]): Promise<void> {
    for (const subTask of subTasks) {
      await this.saveSubTask(taskId, subTask);
    }
  }

  // ============================================================================
  // 事务支持（简化实现）
  // ============================================================================

  async beginTransaction(): Promise<Transaction> {
    return {
      id: `tx-${Date.now()}`,
      startedAt: Date.now(),
      operations: [],
    };
  }

  async commitTransaction(transaction: Transaction): Promise<void> {
    // 简化实现：SessionManager 自动持久化
  }

  async rollbackTransaction(transaction: Transaction): Promise<void> {
    // 简化实现：需要时可以实现快照回滚
  }

  // ============================================================================
  // 查询接口
  // ============================================================================

  async queryTasks(query: TaskQuery): Promise<Task[]> {
    let tasks = await this.getAllTasks();

    if (query.sessionId) {
      tasks = tasks.filter(t => t.sessionId === query.sessionId);
    }

    if (query.status) {
      const statuses = Array.isArray(query.status) ? query.status : [query.status];
      tasks = tasks.filter(t => statuses.includes(t.status));
    }

    if (query.priority) {
      if (query.priority.min !== undefined) {
        tasks = tasks.filter(t => t.priority >= query.priority!.min!);
      }
      if (query.priority.max !== undefined) {
        tasks = tasks.filter(t => t.priority <= query.priority!.max!);
      }
    }

    if (query.createdAfter) {
      tasks = tasks.filter(t => t.createdAt >= query.createdAfter!);
    }

    if (query.createdBefore) {
      tasks = tasks.filter(t => t.createdAt <= query.createdBefore!);
    }

    // Sort
    if (query.sortBy) {
      tasks.sort((a, b) => {
        const aVal = a[query.sortBy!];
        const bVal = b[query.sortBy!];
        const order = query.sortOrder === 'desc' ? -1 : 1;
        return aVal < bVal ? -order : aVal > bVal ? order : 0;
      });
    }

    // Pagination
    if (query.offset) {
      tasks = tasks.slice(query.offset);
    }
    if (query.limit) {
      tasks = tasks.slice(0, query.limit);
    }

    return tasks;
  }

  async querySubTasks(query: SubTaskQuery): Promise<SubTask[]> {
    const tasks = await this.getAllTasks();
    let subTasks: SubTask[] = [];

    for (const task of tasks) {
      if (query.taskId && task.id !== query.taskId) continue;
      subTasks.push(...task.subTasks);
    }

    if (query.status) {
      const statuses = Array.isArray(query.status) ? query.status : [query.status];
      subTasks = subTasks.filter(st => statuses.includes(st.status));
    }

    if (query.assignedWorker) {
      subTasks = subTasks.filter(st => st.assignedWorker === query.assignedWorker);
    }

    if (query.priority) {
      if (query.priority.min !== undefined) {
        subTasks = subTasks.filter(st => st.priority >= query.priority!.min!);
      }
      if (query.priority.max !== undefined) {
        subTasks = subTasks.filter(st => st.priority <= query.priority!.max!);
      }
    }

    // Pagination
    if (query.offset) {
      subTasks = subTasks.slice(query.offset);
    }
    if (query.limit) {
      subTasks = subTasks.slice(0, query.limit);
    }

    return subTasks;
  }

  // ============================================================================
  // 维护操作
  // ============================================================================

  async cleanup(olderThan: number): Promise<number> {
    const tasks = await this.getAllTasks();
    const toDelete = tasks.filter(t =>
      (t.status === 'completed' || t.status === 'failed' || t.status === 'cancelled') &&
      t.createdAt < olderThan
    );

    for (const task of toDelete) {
      await this.deleteTask(task.id);
    }

    return toDelete.length;
  }

  async getStats(): Promise<RepositoryStats> {
    const tasks = await this.getAllTasks();
    const tasksByStatus: Record<TaskStatus, number> = {
      pending: 0,
      running: 0,
      paused: 0,
      retrying: 0,
      completed: 0,
      failed: 0,
      cancelled: 0,
    };

    const subTasksByStatus: Record<SubTaskStatus, number> = {
      pending: 0,
      running: 0,
      paused: 0,
      retrying: 0,
      completed: 0,
      failed: 0,
      skipped: 0,
      cancelled: 0,
    };

    let totalSubTasks = 0;

    for (const task of tasks) {
      tasksByStatus[task.status]++;
      for (const subTask of task.subTasks) {
        totalSubTasks++;
        subTasksByStatus[subTask.status]++;
      }
    }

    return {
      totalTasks: tasks.length,
      totalSubTasks,
      tasksByStatus,
      subTasksByStatus,
      storageSize: 0, // SessionManager doesn't track storage size
    };
  }
}
