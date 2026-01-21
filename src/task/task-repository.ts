/**
 * TaskRepository - 任务持久化层
 *
 * 职责：
 * - Task/SubTask 数据的持久化存储
 * - 查询接口（按 ID、状态、时间等）
 * - 事务支持
 * - 数据恢复
 */

import { logger, LogCategory } from '../logging';
import { Task, SubTask, TaskStatus, SubTaskStatus } from './types';

// ============================================================================
// 事务接口
// ============================================================================

export interface Transaction {
  id: string;
  startedAt: number;
  operations: TransactionOperation[];
}

export interface TransactionOperation {
  type: 'save_task' | 'save_subtask' | 'delete_task';
  data: any;
}

// ============================================================================
// TaskRepository 接口
// ============================================================================

export interface TaskRepository {
  // ============================================================================
  // Task 操作
  // ============================================================================

  /**
   * 保存 Task
   */
  saveTask(task: Task): Promise<void>;

  /**
   * 获取 Task
   */
  getTask(taskId: string): Promise<Task | null>;

  /**
   * 获取 Session 的所有 Task
   */
  getTasksBySession(sessionId: string): Promise<Task[]>;

  /**
   * 获取指定状态的 Task
   */
  getTasksByStatus(status: TaskStatus): Promise<Task[]>;

  /**
   * 获取所有 Task
   */
  getAllTasks(): Promise<Task[]>;

  /**
   * 删除 Task
   */
  deleteTask(taskId: string): Promise<void>;

  // ============================================================================
  // SubTask 操作
  // ============================================================================

  /**
   * 保存 SubTask
   */
  saveSubTask(taskId: string, subTask: SubTask): Promise<void>;

  /**
   * 获取 SubTask
   */
  getSubTask(taskId: string, subTaskId: string): Promise<SubTask | null>;

  /**
   * 获取 Task 的所有 SubTask
   */
  getSubTasksByTask(taskId: string): Promise<SubTask[]>;

  /**
   * 获取指定状态的 SubTask
   */
  getSubTasksByStatus(status: SubTaskStatus): Promise<SubTask[]>;

  // ============================================================================
  // 批量操作
  // ============================================================================

  /**
   * 批量保存 Task
   */
  saveTasks(tasks: Task[]): Promise<void>;

  /**
   * 批量保存 SubTask
   */
  saveSubTasks(taskId: string, subTasks: SubTask[]): Promise<void>;

  // ============================================================================
  // 事务支持
  // ============================================================================

  /**
   * 开始事务
   */
  beginTransaction(): Promise<Transaction>;

  /**
   * 提交事务
   */
  commitTransaction(transaction: Transaction): Promise<void>;

  /**
   * 回滚事务
   */
  rollbackTransaction(transaction: Transaction): Promise<void>;

  // ============================================================================
  // 查询接口
  // ============================================================================

  /**
   * 查询 Task（支持复杂条件）
   */
  queryTasks(query: TaskQuery): Promise<Task[]>;

  /**
   * 查询 SubTask（支持复杂条件）
   */
  querySubTasks(query: SubTaskQuery): Promise<SubTask[]>;

  // ============================================================================
  // 维护操作
  // ============================================================================

  /**
   * 清理旧数据
   */
  cleanup(olderThan: number): Promise<number>;

  /**
   * 获取统计信息
   */
  getStats(): Promise<RepositoryStats>;
}

// ============================================================================
// 查询接口
// ============================================================================

export interface TaskQuery {
  sessionId?: string;
  status?: TaskStatus | TaskStatus[];
  priority?: { min?: number; max?: number };
  createdAfter?: number;
  createdBefore?: number;
  limit?: number;
  offset?: number;
  sortBy?: 'createdAt' | 'priority' | 'status';
  sortOrder?: 'asc' | 'desc';
}

export interface SubTaskQuery {
  taskId?: string;
  status?: SubTaskStatus | SubTaskStatus[];
  assignedWorker?: string;
  priority?: { min?: number; max?: number };
  limit?: number;
  offset?: number;
}

export interface RepositoryStats {
  totalTasks: number;
  totalSubTasks: number;
  tasksByStatus: Record<TaskStatus, number>;
  subTasksByStatus: Record<SubTaskStatus, number>;
  storageSize: number;
}

// ============================================================================
// 文件存储实现
// ============================================================================

import * as fs from 'fs';
import * as path from 'path';

/**
 * 基于文件系统的 TaskRepository 实现
 */
export class FileTaskRepository implements TaskRepository {
  private workspaceRoot: string;
  private storageDir: string;
  private tasksFile: string;
  private cache: Map<string, Task> = new Map();
  private dirty: boolean = false;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.storageDir = path.join(workspaceRoot, '.multicli', 'tasks');
    this.tasksFile = path.join(this.storageDir, 'tasks.json');
    this.ensureStorageDir();
    this.loadCache();
  }

  private ensureStorageDir(): void {
    if (!fs.existsSync(this.storageDir)) {
      fs.mkdirSync(this.storageDir, { recursive: true });
    }
  }

  private loadCache(): void {
    if (fs.existsSync(this.tasksFile)) {
      try {
        const data = fs.readFileSync(this.tasksFile, 'utf-8');
        const tasks: Task[] = JSON.parse(data);
        for (const task of tasks) {
          this.cache.set(task.id, task);
        }
      } catch (error) {
        logger.error('任务.仓库.缓存_加载_失败', error, LogCategory.TASK);
      }
    }
  }

  private async persist(): Promise<void> {
    if (!this.dirty) return;

    const tasks = Array.from(this.cache.values());
    const data = JSON.stringify(tasks, null, 2);
    fs.writeFileSync(this.tasksFile, data, 'utf-8');
    this.dirty = false;
  }

  async saveTask(task: Task): Promise<void> {
    this.cache.set(task.id, task);
    this.dirty = true;
    await this.persist();
  }

  async getTask(taskId: string): Promise<Task | null> {
    return this.cache.get(taskId) || null;
  }

  async getTasksBySession(sessionId: string): Promise<Task[]> {
    return Array.from(this.cache.values()).filter(t => t.sessionId === sessionId);
  }

  async getTasksByStatus(status: TaskStatus): Promise<Task[]> {
    return Array.from(this.cache.values()).filter(t => t.status === status);
  }

  async getAllTasks(): Promise<Task[]> {
    return Array.from(this.cache.values());
  }

  async deleteTask(taskId: string): Promise<void> {
    this.cache.delete(taskId);
    this.dirty = true;
    await this.persist();
  }

  async saveSubTask(taskId: string, subTask: SubTask): Promise<void> {
    const task = this.cache.get(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const index = task.subTasks.findIndex(st => st.id === subTask.id);
    if (index >= 0) {
      task.subTasks[index] = subTask;
    } else {
      task.subTasks.push(subTask);
    }

    this.dirty = true;
    await this.persist();
  }

  async getSubTask(taskId: string, subTaskId: string): Promise<SubTask | null> {
    const task = this.cache.get(taskId);
    if (!task) return null;
    return task.subTasks.find(st => st.id === subTaskId) || null;
  }

  async getSubTasksByTask(taskId: string): Promise<SubTask[]> {
    const task = this.cache.get(taskId);
    return task?.subTasks || [];
  }

  async getSubTasksByStatus(status: SubTaskStatus): Promise<SubTask[]> {
    const result: SubTask[] = [];
    for (const task of this.cache.values()) {
      result.push(...task.subTasks.filter(st => st.status === status));
    }
    return result;
  }

  async saveTasks(tasks: Task[]): Promise<void> {
    for (const task of tasks) {
      this.cache.set(task.id, task);
    }
    this.dirty = true;
    await this.persist();
  }

  async saveSubTasks(taskId: string, subTasks: SubTask[]): Promise<void> {
    const task = this.cache.get(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    for (const subTask of subTasks) {
      const index = task.subTasks.findIndex(st => st.id === subTask.id);
      if (index >= 0) {
        task.subTasks[index] = subTask;
      } else {
        task.subTasks.push(subTask);
      }
    }

    this.dirty = true;
    await this.persist();
  }

  async beginTransaction(): Promise<Transaction> {
    return {
      id: `tx-${Date.now()}`,
      startedAt: Date.now(),
      operations: [],
    };
  }

  async commitTransaction(transaction: Transaction): Promise<void> {
    // Simple implementation: just persist
    await this.persist();
  }

  async rollbackTransaction(transaction: Transaction): Promise<void> {
    // Simple implementation: reload from disk
    this.cache.clear();
    this.loadCache();
    this.dirty = false;
  }

  async queryTasks(query: TaskQuery): Promise<Task[]> {
    let tasks = Array.from(this.cache.values());

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
    let subTasks: SubTask[] = [];

    for (const task of this.cache.values()) {
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

  async cleanup(olderThan: number): Promise<number> {
    const tasks = Array.from(this.cache.values());
    const toDelete = tasks.filter(t =>
      (t.status === 'completed' || t.status === 'failed' || t.status === 'cancelled') &&
      t.createdAt < olderThan
    );

    for (const task of toDelete) {
      this.cache.delete(task.id);
    }

    if (toDelete.length > 0) {
      this.dirty = true;
      await this.persist();
    }

    return toDelete.length;
  }

  async getStats(): Promise<RepositoryStats> {
    const tasks = Array.from(this.cache.values());
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

    let storageSize = 0;
    if (fs.existsSync(this.tasksFile)) {
      storageSize = fs.statSync(this.tasksFile).size;
    }

    return {
      totalTasks: tasks.length,
      totalSubTasks,
      tasksByStatus,
      subTasksByStatus,
      storageSize,
    };
  }
}
