/**
 * 任务状态管理器
 * 负责追踪所有子任务的执行状态，支持持久化和实时同步
 */

import * as fs from 'fs';
import * as path from 'path';
import { CLIType } from '../types';
import { globalEventBus } from '../events';

/** 任务状态类型 */
export type TaskStatus =
  | 'pending'    // 等待执行
  | 'running'    // 执行中
  | 'completed'  // 已完成
  | 'failed'     // 失败
  | 'retrying'   // 重试中
  | 'cancelled'; // 已取消

/** 任务状态 */
export interface TaskState {
  id: string;
  parentTaskId: string;
  description: string;
  assignedCli: CLIType;
  status: TaskStatus;
  progress: number;        // 0-100
  attempts: number;        // 重试次数
  maxAttempts: number;     // 最大重试次数
  startedAt?: number;
  completedAt?: number;
  result?: string;
  error?: string;
  modifiedFiles?: string[];
}

/** 持久化的任务数据 */
interface PersistedTaskData {
  sessionId: string;
  createdAt: number;
  updatedAt: number;
  tasks: TaskState[];
}

/** 状态变更回调 */
export type StateChangeCallback = (task: TaskState, allTasks: TaskState[]) => void;

/**
 * 任务状态管理器
 */
export class TaskStateManager {
  private tasks: Map<string, TaskState> = new Map();
  private sessionId: string;
  private workspaceRoot: string;
  private callbacks: StateChangeCallback[] = [];
  private autoSave: boolean;

  constructor(sessionId: string, workspaceRoot: string, autoSave = true) {
    this.sessionId = sessionId;
    this.workspaceRoot = workspaceRoot;
    this.autoSave = autoSave;
  }

  /** 创建新任务 */
  createTask(params: {
    id: string;
    parentTaskId: string;
    description: string;
    assignedCli: CLIType;
    maxAttempts?: number;
  }): TaskState {
    const task: TaskState = {
      id: params.id,
      parentTaskId: params.parentTaskId,
      description: params.description,
      assignedCli: params.assignedCli,
      status: 'pending',
      progress: 0,
      attempts: 0,
      maxAttempts: params.maxAttempts ?? 3,
    };

    this.tasks.set(task.id, task);
    this.notifyChange(task);
    this.autoSaveIfEnabled();

    return task;
  }

  /** 更新任务状态 */
  updateStatus(taskId: string, status: TaskStatus, error?: string): void {
    const task = this.tasks.get(taskId);
    if (!task) {
      console.warn(`[TaskStateManager] 任务不存在: ${taskId}`);
      return;
    }

    task.status = status;
    if (error) task.error = error;

    if (status === 'running' && !task.startedAt) {
      task.startedAt = Date.now();
    }
    if (status === 'completed' || status === 'failed') {
      task.completedAt = Date.now();
    }
    if (status === 'retrying') {
      task.attempts += 1;
    }

    this.notifyChange(task);
    this.autoSaveIfEnabled();

    // 发送事件
    globalEventBus.emitEvent('task:state_changed', {
      taskId,
      data: { task, allTasks: this.getAllTasks() }
    });
  }

  /** 更新任务进度 */
  updateProgress(taskId: string, progress: number): void {
    const task = this.tasks.get(taskId);
    if (!task) return;

    task.progress = Math.min(100, Math.max(0, progress));
    this.notifyChange(task);
  }

  /** 设置任务结果 */
  setResult(taskId: string, result: string, modifiedFiles?: string[]): void {
    const task = this.tasks.get(taskId);
    if (!task) return;

    task.result = result;
    if (modifiedFiles) task.modifiedFiles = modifiedFiles;
    this.autoSaveIfEnabled();
  }

  /** 获取单个任务 */
  getTask(taskId: string): TaskState | null {
    return this.tasks.get(taskId) ?? null;
  }

  /** 获取所有任务 */
  getAllTasks(): TaskState[] {
    return Array.from(this.tasks.values());
  }

  /** 获取待执行的任务 */
  getPendingTasks(cli?: CLIType): TaskState[] {
    return this.getAllTasks().filter(t => {
      if (t.status !== 'pending') return false;
      if (cli && t.assignedCli !== cli) return false;
      return true;
    });
  }

  /** 获取指定 CLI 的任务 */
  getTasksByCli(cli: CLIType): TaskState[] {
    return this.getAllTasks().filter(t => t.assignedCli === cli);
  }

  /** 检查是否所有任务都已完成 */
  isAllCompleted(): boolean {
    return this.getAllTasks().every(t =>
      t.status === 'completed' || t.status === 'cancelled'
    );
  }

  /** 检查是否有失败的任务 */
  hasFailedTasks(): boolean {
    return this.getAllTasks().some(t => t.status === 'failed');
  }

  /** 获取失败的任务 */
  getFailedTasks(): TaskState[] {
    return this.getAllTasks().filter(t => t.status === 'failed');
  }

  /** 检查任务是否可以重试 */
  canRetry(taskId: string): boolean {
    const task = this.tasks.get(taskId);
    if (!task) return false;
    return task.attempts < task.maxAttempts;
  }

  /** 重置任务为待执行状态（用于重试） */
  resetForRetry(taskId: string): void {
    const task = this.tasks.get(taskId);
    if (!task) return;

    task.status = 'retrying';
    task.attempts += 1;
    task.error = undefined;
    task.result = undefined;
    task.progress = 0;

    this.notifyChange(task);
    this.autoSaveIfEnabled();
  }

  /** 注册状态变更回调 */
  onStateChange(callback: StateChangeCallback): () => void {
    this.callbacks.push(callback);
    return () => {
      const index = this.callbacks.indexOf(callback);
      if (index > -1) this.callbacks.splice(index, 1);
    };
  }

  /** 通知状态变更 */
  private notifyChange(task: TaskState): void {
    const allTasks = this.getAllTasks();
    for (const callback of this.callbacks) {
      try {
        callback(task, allTasks);
      } catch (error) {
        console.error('[TaskStateManager] 回调执行失败:', error);
      }
    }
  }

  /** 自动保存（如果启用） */
  private autoSaveIfEnabled(): void {
    if (this.autoSave) {
      this.save().catch(err => {
        console.error('[TaskStateManager] 自动保存失败:', err);
      });
    }
  }

  /** 获取存储路径 */
  private getStoragePath(): string {
    return path.join(this.workspaceRoot, '.cli-arranger', 'tasks', `${this.sessionId}.json`);
  }

  /** 保存到文件 */
  async save(): Promise<void> {
    const storagePath = this.getStoragePath();
    const dir = path.dirname(storagePath);

    // 确保目录存在
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }

    const data: PersistedTaskData = {
      sessionId: this.sessionId,
      createdAt: Date.now(),
      updatedAt: Date.now(),
      tasks: this.getAllTasks(),
    };

    fs.writeFileSync(storagePath, JSON.stringify(data, null, 2), 'utf-8');
  }

  /** 从文件加载 */
  async load(): Promise<void> {
    const storagePath = this.getStoragePath();

    if (!fs.existsSync(storagePath)) {
      return;
    }

    try {
      const content = fs.readFileSync(storagePath, 'utf-8');
      const data: PersistedTaskData = JSON.parse(content);

      this.tasks.clear();
      for (const task of data.tasks) {
        this.tasks.set(task.id, task);
      }
    } catch (error) {
      console.error('[TaskStateManager] 加载失败:', error);
    }
  }

  /** 清除所有任务 */
  clear(): void {
    this.tasks.clear();
    this.autoSaveIfEnabled();
  }

  /** 获取统计信息 */
  getStats(): {
    total: number;
    pending: number;
    running: number;
    completed: number;
    failed: number;
    cancelled: number;
  } {
    const tasks = this.getAllTasks();
    return {
      total: tasks.length,
      pending: tasks.filter(t => t.status === 'pending').length,
      running: tasks.filter(t => t.status === 'running' || t.status === 'retrying').length,
      completed: tasks.filter(t => t.status === 'completed').length,
      failed: tasks.filter(t => t.status === 'failed').length,
      cancelled: tasks.filter(t => t.status === 'cancelled').length,
    };
  }
}

