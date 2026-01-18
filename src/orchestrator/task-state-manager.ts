/**
 * 任务状态管理器
 * 负责追踪所有子任务的执行状态，支持持久化和实时同步
 * 
 * @deprecated 此类已被 UnifiedTaskManager 替代，将在未来版本中移除
 * @see UnifiedTaskManager - 使用统一的任务管理器
 * @since v0.1.0
 * @deprecated-since v0.7.0
 * 
 * 迁移指南：
 * - 使用 UnifiedTaskManager 替代 TaskStateManager
 * - 使用 SubTask 类型替代 TaskState 类型
 * - 参考 docs/Stage3-完成总结.md 了解迁移详情
 */

import { logger, LogCategory } from '../logging';
import * as fs from 'fs';
import * as path from 'path';
import { CLIType, TaskStatus } from '../types';
import { globalEventBus } from '../events';

/** 任务状态 */
export interface TaskState {
  id: string;
  parentTaskId: string;
  description: string;
  assignedWorker: CLIType;
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
  version: number;
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
/**
 * @deprecated 使用 UnifiedTaskManager 替代
 */
export class TaskStateManager {
  private static readonly STORAGE_VERSION = 1;
  private tasks: Map<string, TaskState> = new Map();
  private sessionId: string;
  private workspaceRoot: string;
  private callbacks: StateChangeCallback[] = [];
  private autoSave: boolean;
  private createdAt: number;
  private updatedAt: number;

  constructor(sessionId: string, workspaceRoot: string, autoSave = true) {
    this.sessionId = sessionId;
    this.workspaceRoot = workspaceRoot;
    this.autoSave = autoSave;
    this.createdAt = Date.now();
    this.updatedAt = this.createdAt;
  }

  /** 创建新任务 */
  createTask(params: {
    id: string;
    parentTaskId: string;
    description: string;
    assignedWorker: CLIType;
    maxAttempts?: number;
  }): TaskState {
    if (this.tasks.has(params.id)) {
      logger.warn(`[TaskStateManager] 任务已存在: ${params.id}`, undefined, LogCategory.ORCHESTRATOR);
      return this.tasks.get(params.id)!;
    }

    const task: TaskState = {
      id: params.id,
      parentTaskId: params.parentTaskId,
      description: params.description,
      assignedWorker: params.assignedWorker,
      status: 'pending',
      progress: 0,
      attempts: 0,
      maxAttempts: params.maxAttempts ?? 3,
    };

    this.tasks.set(task.id, task);
    this.notifyChange(task);
    this.autoSaveIfEnabled();
    this.emitStateChanged(task);

    return task;
  }

  /** 更新任务状态 */
  updateStatus(taskId: string, status: TaskStatus, error?: string): void {
    const task = this.tasks.get(taskId);
    if (!task) {
      logger.warn(`[TaskStateManager] 任务不存在: ${taskId}`, undefined, LogCategory.ORCHESTRATOR);
      return;
    }

    if (!this.applyStatus(task, status, { error })) {
      return;
    }

    this.notifyChange(task);
    this.autoSaveIfEnabled();
    this.emitStateChanged(task);
  }

  /** 更新任务进度 */
  updateProgress(taskId: string, progress: number): void {
    const task = this.tasks.get(taskId);
    if (!task) return;
    if (task.status !== 'running' && task.status !== 'retrying') return;

    task.progress = Math.min(100, Math.max(0, progress));
    this.notifyChange(task);
    this.emitStateChanged(task);
  }

  /** 设置任务结果 */
  setResult(taskId: string, result: string, modifiedFiles?: string[]): void {
    const task = this.tasks.get(taskId);
    if (!task) return;

    task.result = result;
    if (modifiedFiles) task.modifiedFiles = modifiedFiles;
    this.autoSaveIfEnabled();
    this.emitStateChanged(task);
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
      if (cli && t.assignedWorker !== cli) return false;
      return true;
    });
  }

  /** 获取指定 CLI 的任务 */
  getTasksByCli(cli: CLIType): TaskState[] {
    return this.getAllTasks().filter(t => t.assignedWorker === cli);
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

    this.applyStatus(task, 'retrying', { force: true, reset: true, incrementAttempt: true });

    this.notifyChange(task);
    this.autoSaveIfEnabled();
    this.emitStateChanged(task);
  }

  /** 发送状态变更事件 */
  private emitStateChanged(task: TaskState): void {
    globalEventBus.emitEvent('task:state_changed', {
      taskId: task.id,
      data: { task, allTasks: this.getAllTasks() }
    });
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
        logger.error('[TaskStateManager] 回调执行失败:', error, LogCategory.ORCHESTRATOR);
      }
    }
  }

  /** 自动保存（如果启用） */
  private autoSaveIfEnabled(): void {
    if (this.autoSave) {
      this.save().catch(err => {
        logger.error('[TaskStateManager] 自动保存失败:', err, LogCategory.ORCHESTRATOR);
      });
    }
  }

  /** 获取存储路径 */
  private getStoragePath(): string {
    return path.join(this.workspaceRoot, '.multicli', 'tasks', `${this.sessionId}.json`);
  }

  /** 保存到文件 */
  async save(): Promise<void> {
    const storagePath = this.getStoragePath();
    const dir = path.dirname(storagePath);

    // 确保目录存在
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }

    this.updatedAt = Date.now();
    const data: PersistedTaskData = {
      version: TaskStateManager.STORAGE_VERSION,
      sessionId: this.sessionId,
      createdAt: this.createdAt,
      updatedAt: this.updatedAt,
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
      const data = JSON.parse(content) as Partial<PersistedTaskData>;
      const tasks = Array.isArray(data.tasks) ? data.tasks : [];
      this.tasks.clear();
      for (const raw of tasks) {
        const normalized = this.normalizeTaskState(raw);
        if (normalized) {
          this.tasks.set(normalized.id, normalized);
        }
      }
      if (typeof data.createdAt === 'number') {
        this.createdAt = data.createdAt;
      }
      if (typeof data.updatedAt === 'number') {
        this.updatedAt = data.updatedAt;
      }
      for (const task of this.tasks.values()) {
        this.notifyChange(task);
        this.emitStateChanged(task);
      }
    } catch (error) {
      logger.error('[TaskStateManager] 加载失败:', error, LogCategory.ORCHESTRATOR);
    }
  }

  /** 清除所有任务 */
  clear(): void {
    this.tasks.clear();
    this.createdAt = Date.now();
    this.updatedAt = this.createdAt;
    const storagePath = this.getStoragePath();
    if (fs.existsSync(storagePath)) {
      fs.unlinkSync(storagePath);
    }
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

  private applyStatus(
    task: TaskState,
    status: TaskStatus,
    options: {
      error?: string;
      force?: boolean;
      reset?: boolean;
      incrementAttempt?: boolean;
    } = {}
  ): boolean {
    const prevStatus = task.status;
    if (!options.force && !this.isTransitionAllowed(prevStatus, status)) {
      logger.warn(`[TaskStateManager] 非法状态流转: ${prevStatus} -> ${status}`, undefined, LogCategory.ORCHESTRATOR);
      return false;
    }
    task.status = status;
    if (options.reset) {
      task.error = undefined;
      task.result = undefined;
      task.progress = 0;
      task.startedAt = undefined;
      task.completedAt = undefined;
    }
    if (typeof options.error === 'string') {
      task.error = options.error;
    }
    if (status === 'running' && !task.startedAt) {
      task.startedAt = Date.now();
    }
    if (status === 'completed' || status === 'failed' || status === 'cancelled') {
      task.completedAt = Date.now();
    }
    if (status === 'retrying' && options.incrementAttempt) {
      task.attempts += 1;
    }
    if (status === 'completed' && task.progress < 100) {
      task.progress = 100;
    }
    return true;
  }

  private isTransitionAllowed(from: TaskStatus, to: TaskStatus): boolean {
    if (from === to) return true;
    const allowed: Record<TaskStatus, TaskStatus[]> = {
      pending: ['running', 'retrying', 'paused', 'failed', 'cancelled', 'completed'],
      running: ['completed', 'failed', 'retrying', 'paused', 'cancelled'],
      paused: ['running', 'cancelled'],
      retrying: ['running', 'failed', 'cancelled', 'completed'],
      failed: ['retrying', 'cancelled'],
      completed: [],
      cancelled: [],
    };
    return allowed[from].includes(to);
  }

  private normalizeTaskState(raw: Partial<TaskState>): TaskState | null {
    if (!raw || typeof raw !== 'object') return null;
    if (typeof raw.id !== 'string' || typeof raw.parentTaskId !== 'string' || typeof raw.description !== 'string') {
      return null;
    }
    if (raw.assignedWorker !== 'claude' && raw.assignedWorker !== 'codex' && raw.assignedWorker !== 'gemini') {
      return null;
    }
    const status = this.normalizeStatus(raw.status);
    if (!status) return null;
    const progress = this.clampNumber(raw.progress ?? 0, 0, 100);
    const attempts = Math.max(0, Number(raw.attempts ?? 0));
    const maxAttempts = Math.max(1, Number(raw.maxAttempts ?? 3));
    const startedAt = typeof raw.startedAt === 'number' ? raw.startedAt : undefined;
    const completedAt = typeof raw.completedAt === 'number' ? raw.completedAt : undefined;
    const result = typeof raw.result === 'string' ? raw.result : undefined;
    const error = typeof raw.error === 'string' ? raw.error : undefined;
    const modifiedFiles = Array.isArray(raw.modifiedFiles)
      ? raw.modifiedFiles.filter(file => typeof file === 'string')
      : undefined;

    const normalized: TaskState = {
      id: raw.id,
      parentTaskId: raw.parentTaskId,
      description: raw.description,
      assignedWorker: raw.assignedWorker,
      status,
      progress: status === 'completed' ? 100 : progress,
      attempts,
      maxAttempts,
      startedAt: status === 'pending' ? undefined : startedAt,
      completedAt: status === 'completed' || status === 'failed' || status === 'cancelled' ? completedAt : undefined,
      result,
      error,
      modifiedFiles,
    };
    return normalized;
  }

  private normalizeStatus(status?: TaskStatus): TaskStatus | null {
    if (!status) return null;
    const allowed: TaskStatus[] = ['pending', 'running', 'completed', 'failed', 'retrying', 'cancelled'];
    return allowed.includes(status) ? status : null;
  }

  private clampNumber(value: number, min: number, max: number): number {
    const normalized = Number(value);
    if (Number.isNaN(normalized)) return min;
    return Math.min(max, Math.max(min, normalized));
  }
}
