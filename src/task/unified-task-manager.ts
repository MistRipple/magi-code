/**
 * UnifiedTaskManager - 统一任务管理器
 *
 * 职责：
 * - Task/SubTask 完整生命周期管理
 * - 优先级调度（PriorityQueue）
 * - 超时管理（TimeoutChecker）
 * - 持久化（TaskRepository）
 * - 暂停/恢复/重试功能
 *
 * 设计原则：
 * - 单一职责：只管理任务状态和生命周期
 * - 事件驱动：通过 EventBus 通知外部
 * - 持久化优先：所有状态变更立即持久化
 */

import { logger, LogCategory } from '../logging';
import { EventEmitter } from 'events';
import {
  Task,
  SubTask,
  TaskStatus,
  SubTaskStatus,
  CreateTaskParams,
  CreateSubTaskParams,
  WorkerResult,
} from './types';
import { TaskRepository } from './task-repository';
import { PriorityQueue, PriorityItem } from './priority-queue';
import { TimeoutChecker } from './timeout-checker';

// ============================================================================
// 辅助类型
// ============================================================================

interface TaskPriorityItem extends PriorityItem {
  id: string;
  priority: number;
  taskId: string;
}

interface SubTaskPriorityItem extends PriorityItem {
  id: string;
  priority: number;
  taskId: string;
  subTaskId: string;
}

// ============================================================================
// 事件类型
// ============================================================================

export interface TaskManagerEvents {
  // Task 事件
  'task:created': (task: Task) => void;
  'task:started': (task: Task) => void;
  'task:paused': (task: Task) => void;
  'task:resumed': (task: Task) => void;
  'task:completed': (task: Task) => void;
  'task:failed': (task: Task) => void;
  'task:cancelled': (task: Task) => void;
  'task:timeout': (task: Task) => void;
  'task:plan-updated': (task: Task) => void;
  'task:plan-status-updated': (task: Task) => void;

  // SubTask 事件
  'subtask:created': (task: Task, subTask: SubTask) => void;
  'subtask:started': (task: Task, subTask: SubTask) => void;
  'subtask:paused': (task: Task, subTask: SubTask) => void;
  'subtask:resumed': (task: Task, subTask: SubTask) => void;
  'subtask:retrying': (task: Task, subTask: SubTask) => void;
  'subtask:progress': (task: Task, subTask: SubTask, progress: number) => void;
  'subtask:completed': (task: Task, subTask: SubTask) => void;
  'subtask:failed': (task: Task, subTask: SubTask) => void;
  'subtask:skipped': (task: Task, subTask: SubTask) => void;
  'subtask:cancelled': (task: Task, subTask: SubTask) => void;
  'subtask:timeout': (task: Task, subTask: SubTask) => void;
}

// ============================================================================
// UnifiedTaskManager
// ============================================================================

export class UnifiedTaskManager extends EventEmitter {
  private repository: TaskRepository;
  private taskQueue: PriorityQueue<TaskPriorityItem>;
  private subTaskQueue: PriorityQueue<SubTaskPriorityItem>;
  private timeoutChecker: TimeoutChecker;
  private sessionId: string;

  /** 缓存大小上限 */
  private static readonly MAX_CACHE_SIZE = 1000;

  /**
   * 内存缓存（用于快速访问，带 LRU 淘汰）
   * 利用 ES6 Map 的插入顺序特性实现 O(1) LRU：
   * - 最早插入的在前面（迭代时第一个）
   * - delete + set 会把元素移到最后（最新访问）
   */
  private taskCache: Map<string, Task> = new Map();

  constructor(
    sessionId: string,
    repository: TaskRepository,
    options?: {
      timeoutCheckInterval?: number;
      maxCacheSize?: number;
    }
  ) {
    super();
    this.sessionId = sessionId;
    this.repository = repository;
    this.taskQueue = new PriorityQueue<TaskPriorityItem>();
    this.subTaskQueue = new PriorityQueue<SubTaskPriorityItem>();
    this.timeoutChecker = new TimeoutChecker(options?.timeoutCheckInterval);
  }

  /**
   * 缓存任务（带 LRU 淘汰）
   * 利用 ES6 Map 插入顺序特性实现 O(1) 操作
   */
  private cacheTask(task: Task): void {
    const taskId = task.id;

    // 如果已存在，先删除（delete + set 会移到末尾）
    if (this.taskCache.has(taskId)) {
      this.taskCache.delete(taskId);
    }

    // 添加到缓存（插入到末尾 = 最新访问）
    this.taskCache.set(taskId, task);

    // LRU 淘汰：超过上限时移除最久未访问的任务（Map 迭代顺序 = 插入顺序）
    while (this.taskCache.size > UnifiedTaskManager.MAX_CACHE_SIZE) {
      const oldestId = this.taskCache.keys().next().value;
      if (oldestId) {
        this.taskCache.delete(oldestId);
      } else {
        break;
      }
    }
  }

  /**
   * 从缓存获取任务（更新访问顺序）
   * 利用 ES6 Map delete + set 实现 O(1) 访问顺序更新
   */
  private getCachedTask(taskId: string): Task | undefined {
    const task = this.taskCache.get(taskId);
    if (task) {
      // 更新访问顺序：delete + set 移到末尾
      this.taskCache.delete(taskId);
      this.taskCache.set(taskId, task);
    }
    return task;
  }

  /**
   * 初始化（从持久化层恢复状态）
   */
  async initialize(): Promise<void> {
    const tasks = await this.repository.getTasksBySession(this.sessionId);
    for (const task of tasks) {
      this.cacheTask(task);

      // 恢复到优先级队列
      if (task.status === 'pending') {
        this.taskQueue.enqueue({
          id: task.id,
          taskId: task.id,
          priority: task.priority,
        });
      }

      // 恢复 SubTask 到队列
      for (const subTask of task.subTasks) {
        if (subTask.status === 'pending') {
          this.subTaskQueue.enqueue({
            id: subTask.id,
            taskId: task.id,
            subTaskId: subTask.id,
            priority: subTask.priority,
          });
        }
      }

      // 恢复超时监控
      if (task.timeoutAt && task.status === 'running') {
        this.timeoutChecker.add(task.id, task.timeoutAt, () => {
          this.handleTaskTimeout(task.id);
        });
      }

      for (const subTask of task.subTasks) {
        if (subTask.timeoutAt && subTask.status === 'running') {
          this.timeoutChecker.add(subTask.id, subTask.timeoutAt, () => {
            this.handleSubTaskTimeout(task.id, subTask.id);
          });
        }
      }
    }
  }

  /**
   * 销毁管理器
   */
  destroy(): void {
    this.timeoutChecker.destroy();
    this.taskCache.clear();
    this.taskQueue.clear();
    this.subTaskQueue.clear();
    this.removeAllListeners();
  }

  // ============================================================================
  // Task 生命周期管理
  // ============================================================================

  /**
   * 创建 Task
   */
  async createTask(params: CreateTaskParams): Promise<Task> {
    const task: Task = {
      id: params.id || this.generateId(),
      sessionId: this.sessionId,
      prompt: params.prompt,
      missionId: params.missionId,
      status: 'pending',
      priority: params.priority ?? 5,
      subTasks: [],
      createdAt: Date.now(),
      retryCount: 0,
      maxRetries: params.maxRetries ?? 3,
      timeout: params.timeout,
      timeoutAt: params.timeout ? Date.now() + params.timeout : undefined,
    };

    // 持久化
    await this.repository.saveTask(task);

    // 缓存
    this.cacheTask(task);

    // 加入优先级队列
    this.taskQueue.enqueue({
      id: task.id,
      taskId: task.id,
      priority: task.priority,
    });

    // 设置超时监控
    if (task.timeoutAt) {
      this.timeoutChecker.add(task.id, task.timeoutAt, () => {
        this.handleTaskTimeout(task.id);
      });
    }

    // 发送事件
    this.emit('task:created', task);

    return task;
  }

  /**
   * 获取 Task
   */
  async getTask(taskId: string): Promise<Task | null> {
    // 先从缓存获取（使用 LRU 访问）
    const cachedTask = this.getCachedTask(taskId);
    if (cachedTask) return cachedTask;

    // 从持久化层获取
    const task = await this.repository.getTask(taskId);
    if (task) {
      this.cacheTask(task);
    }
    return task;
  }

  /**
   * 获取所有 Task
   */
  async getAllTasks(): Promise<Task[]> {
    return await this.repository.getTasksBySession(this.sessionId);
  }

  /**
   * 获取下一个待执行的 Task（不移除）
   */
  getNextPendingTask(): Task | null {
    const item = this.taskQueue.peek();
    if (!item) return null;
    return this.taskCache.get(item.taskId) || null;
  }

  /**
   * 出队并返回优先级最高的待执行 Task
   * 使用 dequeue() 原子操作，避免 peek + remove 的竞态条件
   */
  dequeueTask(): Task | null {
    const item = this.taskQueue.dequeue();
    if (!item) return null;
    return this.taskCache.get(item.taskId) || null;
  }

  /**
   * 启动 Task
   */
  async startTask(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    if (task.status !== 'pending' && task.status !== 'paused') {
      throw new Error(`Cannot start task in status: ${task.status}`);
    }

    const wasResumed = task.status === 'paused';

    task.status = 'running';
    if (!task.startedAt) {
      task.startedAt = Date.now();
    }
    task.pausedAt = undefined;

    // 从队列中移除
    this.taskQueue.remove(taskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit(wasResumed ? 'task:resumed' : 'task:started', task);
  }

  /**
   * 暂停 Task
   */
  async pauseTask(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    if (task.status !== 'running') {
      throw new Error(`Cannot pause task in status: ${task.status}`);
    }

    task.status = 'paused';
    task.pausedAt = Date.now();

    // 移除超时监控
    this.timeoutChecker.remove(taskId);

    // 暂停所有运行中的 SubTask
    for (const subTask of task.subTasks) {
      if (subTask.status === 'running') {
        await this.pauseSubTask(taskId, subTask.id);
      }
    }

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('task:paused', task);
  }

  /**
   * 恢复 Task
   */
  async resumeTask(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    if (task.status !== 'paused') {
      throw new Error(`Cannot resume task in status: ${task.status}`);
    }

    // 重新加入队列
    this.taskQueue.enqueue({
      id: task.id,
      taskId: task.id,
      priority: task.priority,
    });

    // 恢复超时监控
    if (task.timeoutAt) {
      this.timeoutChecker.add(task.id, task.timeoutAt, () => {
        this.handleTaskTimeout(task.id);
      });
    }

    // 启动任务
    await this.startTask(taskId);
  }

  /**
   * 完成 Task
   */
  async completeTask(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    task.status = 'completed';
    task.completedAt = Date.now();

    // 移除超时监控
    this.timeoutChecker.remove(taskId);

    // 从队列中移除
    this.taskQueue.remove(taskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('task:completed', task);
  }

  /**
   * 失败 Task
   */
  async failTask(taskId: string, error?: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    task.status = 'failed';
    task.completedAt = Date.now();

    // 移除超时监控
    this.timeoutChecker.remove(taskId);

    // 从队列中移除
    this.taskQueue.remove(taskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('task:failed', task);
  }

  /**
   * 取消 Task
   */
  async cancelTask(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    task.status = 'cancelled';
    task.cancelledAt = Date.now();

    // 移除超时监控
    this.timeoutChecker.remove(taskId);

    // 从队列中移除
    this.taskQueue.remove(taskId);

    // 取消所有 SubTask
    for (const subTask of task.subTasks) {
      if (subTask.status === 'pending' || subTask.status === 'running') {
        subTask.status = 'skipped';
        this.timeoutChecker.remove(subTask.id);
        this.subTaskQueue.remove(subTask.id);
      }
    }

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('task:cancelled', task);
  }

  /**
   * 重试 Task
   */
  async retryTask(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    if (task.status !== 'failed') {
      throw new Error(`Cannot retry task in status: ${task.status}`);
    }

    if (task.retryCount >= task.maxRetries) {
      throw new Error(`Task has reached max retries: ${task.maxRetries}`);
    }

    task.status = 'retrying';
    task.retryCount++;

    // 重置所有失败的 SubTask
    for (const subTask of task.subTasks) {
      if (subTask.status === 'failed') {
        subTask.status = 'pending';
        subTask.retryCount++;
        subTask.error = undefined;
        subTask.startedAt = undefined;
        subTask.completedAt = undefined;

        // 重新加入队列
        this.subTaskQueue.enqueue({
          id: subTask.id,
          taskId: task.id,
          subTaskId: subTask.id,
          priority: subTask.priority,
        });
      }
    }

    // 持久化
    await this.repository.saveTask(task);

    // 重新启动
    await this.startTask(taskId);
  }

  /**
   * 处理 Task 超时
   */
  private async handleTaskTimeout(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) return;

    if (task.status === 'running') {
      task.status = 'failed';
      task.completedAt = Date.now();

      // 从队列中移除
      this.taskQueue.remove(taskId);

      // 持久化
      await this.repository.saveTask(task);

      // 发送事件
      this.emit('task:timeout', task);
    }
  }

  // ============================================================================
  // SubTask 生命周期管理
  // ============================================================================

  /**
   * 创建 SubTask
   */
  async createSubTask(taskId: string, params: CreateSubTaskParams): Promise<SubTask> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask: SubTask = {
      id: this.generateId(),
      taskId,
      description: params.description,
      title: params.description.substring(0, 50),
      assignmentId: params.assignmentId,
      assignedWorker: params.assignedWorker,
      reason: params.reason,
      prompt: params.prompt,
      targetFiles: params.targetFiles || [],
      dependencies: params.dependencies || [],
      priority: params.priority ?? 5,
      kind: params.kind,
      background: params.background,
      status: 'pending',
      progress: 0,
      retryCount: 0,
      maxRetries: params.maxRetries ?? 3,
      timeout: params.timeout,
      timeoutAt: params.timeout ? Date.now() + params.timeout : undefined,
      output: [],
    };

    // 添加到 Task
    task.subTasks.push(subTask);

    // 持久化
    await this.repository.saveTask(task);

    // 加入优先级队列
    this.subTaskQueue.enqueue({
      id: subTask.id,
      taskId,
      subTaskId: subTask.id,
      priority: subTask.priority,
    });

    // 设置超时监控
    if (subTask.timeoutAt) {
      this.timeoutChecker.add(subTask.id, subTask.timeoutAt, () => {
        this.handleSubTaskTimeout(taskId, subTask.id);
      });
    }

    // 发送事件
    this.emit('subtask:created', task, subTask);

    return subTask;
  }

  /**
   * 获取 SubTask
   */
  async getSubTask(taskId: string, subTaskId: string): Promise<SubTask | null> {
    const task = await this.getTask(taskId);
    if (!task) return null;
    return task.subTasks.find(st => st.id === subTaskId) || null;
  }

  /**
   * 根据 Assignment ID 获取 SubTask
   * 用于 Mission 执行时的稳定匹配
   */
  async getSubTaskByAssignmentId(taskId: string, assignmentId: string): Promise<SubTask | null> {
    const task = await this.getTask(taskId);
    if (!task) return null;
    return task.subTasks.find(st => st.assignmentId === assignmentId) || null;
  }

  /**
   * 更新 SubTask（通用更新方法）
   */
  async updateSubTask(taskId: string, updates: Partial<SubTask> & { id: string }): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === updates.id);
    if (!subTask) throw new Error(`SubTask not found: ${updates.id}`);

    Object.assign(subTask, updates);

    // 持久化
    await this.repository.saveTask(task);
  }

  /**
   * 获取下一个待执行的 SubTask（不移除）
   */
  getNextPendingSubTask(): { task: Task; subTask: SubTask } | null {
    const item = this.subTaskQueue.peek();
    if (!item) return null;

    const task = this.taskCache.get(item.taskId);
    if (!task) return null;

    const subTask = task.subTasks.find(st => st.id === item.subTaskId);
    if (!subTask) return null;

    return { task, subTask };
  }

  /**
   * 出队并返回优先级最高的待执行 SubTask
   * 使用 dequeue() 原子操作，避免 peek + remove 的竞态条件
   */
  dequeueSubTask(): { task: Task; subTask: SubTask } | null {
    const item = this.subTaskQueue.dequeue();
    if (!item) return null;

    const task = this.taskCache.get(item.taskId);
    if (!task) return null;

    const subTask = task.subTasks.find(st => st.id === item.subTaskId);
    if (!subTask) return null;

    return { task, subTask };
  }

  /**
   * 批量出队多个 SubTask（按优先级顺序）
   * 用于并行执行多个高优先级任务
   */
  dequeueBatchSubTasks(count: number): Array<{ task: Task; subTask: SubTask }> {
    const results: Array<{ task: Task; subTask: SubTask }> = [];

    for (let i = 0; i < count; i++) {
      const result = this.dequeueSubTask();
      if (!result) break;
      results.push(result);
    }

    return results;
  }

  /**
   * 启动 SubTask
   */
  async startSubTask(taskId: string, subTaskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    if (subTask.status !== 'pending' && subTask.status !== 'paused' && subTask.status !== 'retrying') {
      throw new Error(`Cannot start subtask in status: ${subTask.status}`);
    }

    const wasResumed = subTask.status === 'paused';
    const wasRetrying = subTask.status === 'retrying';

    subTask.status = 'running';
    if (!subTask.startedAt) {
      subTask.startedAt = Date.now();
    }
    subTask.pausedAt = undefined;

    // 从队列中移除
    this.subTaskQueue.remove(subTaskId);

    // 如果 Task 还是 pending，启动它
    if (task.status === 'pending') {
      await this.startTask(taskId);
    }

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    if (wasRetrying) {
      // 重试后启动，不发送新的 started 事件
      this.emit('subtask:resumed', task, subTask);
    } else {
      this.emit(wasResumed ? 'subtask:resumed' : 'subtask:started', task, subTask);
    }
  }

  /**
   * 更新 SubTask 进度
   */
  async updateSubTaskProgress(taskId: string, subTaskId: string, progress: number): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    subTask.progress = Math.max(0, Math.min(100, progress));

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:progress', task, subTask, subTask.progress);
  }

  /**
   * 暂停 SubTask
   */
  async pauseSubTask(taskId: string, subTaskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    if (subTask.status !== 'running') {
      throw new Error(`Cannot pause subtask in status: ${subTask.status}`);
    }

    subTask.status = 'paused';
    subTask.pausedAt = Date.now();

    // 移除超时监控
    this.timeoutChecker.remove(subTaskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:paused', task, subTask);
  }

  /**
   * 恢复 SubTask
   */
  async resumeSubTask(taskId: string, subTaskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    if (subTask.status !== 'paused') {
      throw new Error(`Cannot resume subtask in status: ${subTask.status}`);
    }

    // 重新加入队列
    this.subTaskQueue.enqueue({
      id: subTask.id,
      taskId,
      subTaskId: subTask.id,
      priority: subTask.priority,
    });

    // 恢复超时监控
    if (subTask.timeoutAt) {
      this.timeoutChecker.add(subTask.id, subTask.timeoutAt, () => {
        this.handleSubTaskTimeout(taskId, subTask.id);
      });
    }

    // 启动 SubTask
    await this.startSubTask(taskId, subTaskId);
  }

  /**
   * 完成 SubTask
   */
  async completeSubTask(taskId: string, subTaskId: string, result?: WorkerResult): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    subTask.status = 'completed';
    subTask.progress = 100;
    subTask.completedAt = Date.now();
    subTask.result = result;

    if (result?.modifiedFiles) {
      subTask.modifiedFiles = result.modifiedFiles;
    }

    // 移除超时监控
    this.timeoutChecker.remove(subTaskId);

    // 从队列中移除
    this.subTaskQueue.remove(subTaskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:completed', task, subTask);

    // 检查 Task 是否完成
    await this.checkTaskCompletion(taskId);
  }

  /**
   * 失败 SubTask
   */
  async failSubTask(taskId: string, subTaskId: string, error: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    subTask.status = 'failed';
    subTask.completedAt = Date.now();
    subTask.error = error;

    // 移除超时监控
    this.timeoutChecker.remove(subTaskId);

    // 从队列中移除
    this.subTaskQueue.remove(subTaskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:failed', task, subTask);

    // 检查是否需要重试
    if (subTask.retryCount < subTask.maxRetries) {
      // 可以重试，但不自动重试，等待外部决策
    } else {
      // 已达到最大重试次数，标记 Task 为失败
      await this.failTask(taskId);
    }
  }

  /**
   * 取消 SubTask
   */
  async cancelSubTask(taskId: string, subTaskId: string, reason?: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    // 只有 pending 或 running 状态的 SubTask 可以被取消
    if (subTask.status !== 'pending' && subTask.status !== 'running' && subTask.status !== 'paused') {
      return; // 已经是终态，不需要取消
    }

    subTask.status = 'cancelled';
    subTask.completedAt = Date.now();
    if (reason) {
      subTask.error = reason;
    }

    // 移除超时监控
    this.timeoutChecker.remove(subTaskId);

    // 从队列中移除
    this.subTaskQueue.remove(subTaskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:cancelled', task, subTask);
  }

  /**
   * 检查 SubTask 是否可以重试
   */
  canRetrySubTask(taskId: string, subTaskId: string): boolean {
    const task = this.taskCache.get(taskId);
    if (!task) return false;

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) return false;

    return subTask.retryCount < subTask.maxRetries;
  }

  /**
   * 重置 SubTask 为重试状态
   */
  async resetSubTaskForRetry(taskId: string, subTaskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    // 检查是否可以重试
    if (subTask.retryCount >= subTask.maxRetries) {
      throw new Error(`SubTask ${subTaskId} has reached max retries (${subTask.maxRetries})`);
    }

    // 增加重试计数
    subTask.retryCount += 1;

    // 重置状态
    subTask.status = 'retrying';
    subTask.error = undefined;
    subTask.progress = 0;
    subTask.completedAt = undefined;

    // 重新加入队列
    this.subTaskQueue.enqueue({
      id: subTask.id,
      taskId,
      subTaskId: subTask.id,
      priority: subTask.priority,
    });

    // 恢复超时监控
    if (subTask.timeoutAt) {
      this.timeoutChecker.add(subTask.id, subTask.timeoutAt, () => {
        this.handleSubTaskTimeout(taskId, subTask.id);
      });
    }

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:retrying', task, subTask);

    logger.info(
      '任务.子任务.重试_重置',
      { subTaskId, retryCount: subTask.retryCount, maxRetries: subTask.maxRetries },
      LogCategory.TASK
    );
  }

  /**
   * 跳过 SubTask
   */
  async skipSubTask(taskId: string, subTaskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    subTask.status = 'skipped';
    subTask.completedAt = Date.now();

    // 移除超时监控
    this.timeoutChecker.remove(subTaskId);

    // 从队列中移除
    this.subTaskQueue.remove(subTaskId);

    // 持久化
    await this.repository.saveTask(task);

    // 发送事件
    this.emit('subtask:skipped', task, subTask);

    // 检查 Task 是否完成
    await this.checkTaskCompletion(taskId);
  }

  /**
   * 添加 SubTask 输出
   */
  async addSubTaskOutput(taskId: string, subTaskId: string, output: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    subTask.output.push(output);

    // 持久化
    await this.repository.saveTask(task);
  }

  /**
   * 处理 SubTask 超时
   */
  private async handleSubTaskTimeout(taskId: string, subTaskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) return;

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) return;

    if (subTask.status === 'running') {
      await this.failSubTask(taskId, subTaskId, 'SubTask timeout');
      this.emit('subtask:timeout', task, subTask);
    }
  }

  /**
   * 检查 Task 是否完成
   */
  private async checkTaskCompletion(taskId: string): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task || task.status !== 'running') return;

    const allDone = task.subTasks.every(
      st => st.status === 'completed' || st.status === 'skipped'
    );
    const anyFailed = task.subTasks.some(st => st.status === 'failed');

    if (anyFailed) {
      await this.failTask(taskId);
    } else if (allDone) {
      await this.completeTask(taskId);
    }
  }

  // ============================================================================
  // 辅助方法
  // ============================================================================

  /**
   * 生成唯一 ID
   */
  private generateId(): string {
    return `${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
  }

  /**
   * 更新 Task（通用更新方法）
   */
  async updateTask(taskId: string, updates: Partial<Task>): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    Object.assign(task, updates);

    // 持久化
    await this.repository.saveTask(task);
  }

  /**
   * 更新 Task 关联的执行计划信息
   */
  async updateTaskPlan(
    taskId: string,
    planInfo: { planId: string; planSummary?: string; status?: Task['planStatus'] }
  ): Promise<void> {
    const updates: Partial<Task> = {
      planId: planInfo.planId,
      planSummary: planInfo.planSummary,
      planStatus: planInfo.status ?? 'ready',
      planCreatedAt: Date.now(),
      planUpdatedAt: Date.now(),
    };
    await this.updateTask(taskId, updates);
  }

  /**
   * 更新 Task 的执行计划状态
   */
  async updateTaskPlanStatus(taskId: string, status: Task['planStatus']): Promise<void> {
    await this.updateTask(taskId, {
      planStatus: status,
      planUpdatedAt: Date.now(),
    });
  }

  /**
   * 添加既有 SubTask（用于编排计划落库或状态恢复）
   */
  async addExistingSubTask(taskId: string, subTask: SubTask): Promise<SubTask> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    // 检查是否已存在
    const existing = task.subTasks.find(st => st.id === subTask.id);
    if (existing) {
      return existing;
    }

    // 规范化 SubTask
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
    };

    task.subTasks.push(normalized);

    // 持久化
    await this.repository.saveTask(task);

    // 如果是 pending 状态，加入队列
    if (normalized.status === 'pending') {
      this.subTaskQueue.enqueue({
        id: normalized.id,
        taskId,
        subTaskId: normalized.id,
        priority: normalized.priority,
      });
    }

    return normalized;
  }

  /**
   * 更新 SubTask 的实际修改文件
   */
  async updateSubTaskFiles(taskId: string, subTaskId: string, files: string[]): Promise<void> {
    const task = await this.getTask(taskId);
    if (!task) throw new Error(`Task not found: ${taskId}`);

    const subTask = task.subTasks.find(st => st.id === subTaskId);
    if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

    const normalized = Array.from(
      new Set((files || []).filter(f => typeof f === 'string' && f.trim()))
    );
    subTask.modifiedFiles = normalized;

    // 持久化
    await this.repository.saveTask(task);
  }

  /**
   * 获取统计信息
   */
  async getStats() {
    return await this.repository.getStats();
  }

  /**
   * 清理旧数据
   */
  async cleanup(olderThan: number): Promise<number> {
    return await this.repository.cleanup(olderThan);
  }
}
