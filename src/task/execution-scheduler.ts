/**
 * 执行调度器
 * 支持并行/串行执行策略，管理任务队列
 */

import { EventEmitter } from 'events';
import { CLIType } from '../types';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { CLIResponse } from '../cli/types';
import { SubTaskDef, SplitResult } from './task-splitter';

/** 执行状态 */
export type ExecutionStatus = 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';

/** 子任务执行结果 */
export interface SubTaskResult {
  subTaskId: string;
  cli: CLIType;
  status: ExecutionStatus;
  response?: CLIResponse;
  error?: string;
  startTime: number;
  endTime?: number;
  duration?: number;
}

/** 调度器配置 */
export interface SchedulerConfig {
  maxParallel: number;
  timeout: number;
  retryCount: number;
}

/** 执行快照（用于持久化和恢复） */
export interface ExecutionSnapshot {
  queue: SubTaskDef[];
  results: SubTaskResult[];
  isRunning: boolean;
  isCancelled: boolean;
  timestamp: number;
}

const DEFAULT_CONFIG: SchedulerConfig = {
  maxParallel: 3,
  timeout: 300000,
  retryCount: 1,
};

/**
 * 执行调度器类
 */
export class ExecutionScheduler extends EventEmitter {
  private factory: CLIAdapterFactory;
  private config: SchedulerConfig;
  private queue: SubTaskDef[] = [];
  private running: Map<string, SubTaskDef> = new Map();
  private results: Map<string, SubTaskResult> = new Map();
  private isRunning = false;
  private isCancelled = false;

  constructor(factory: CLIAdapterFactory, config?: Partial<SchedulerConfig>) {
    super();
    this.factory = factory;
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 执行拆分结果
   */
  async execute(splitResult: SplitResult): Promise<SubTaskResult[]> {
    this.reset();
    this.isRunning = true;
    this.queue = [...splitResult.subTasks];

    this.emit('start', { total: this.queue.length, mode: splitResult.executionMode });

    try {
      if (splitResult.executionMode === 'parallel') {
        await this.executeParallel();
      } else {
        await this.executeSequential();
      }
    } catch (error) {
      this.emit('error', error);
    }

    this.isRunning = false;
    const results = Array.from(this.results.values());
    this.emit('complete', { results, cancelled: this.isCancelled });
    return results;
  }

  /**
   * 串行执行
   */
  private async executeSequential(): Promise<void> {
    while (this.queue.length > 0 && !this.isCancelled) {
      const task = this.queue.shift();
      if (!task) break;

      // 检查依赖是否完成
      if (!this.checkDependencies(task)) {
        this.queue.push(task);
        continue;
      }

      const result = await this.executeSubTask(task);
      if (result.status === 'failed') {
        this.emit('taskFailed', { task, result });
        break;
      }
    }
  }

  /**
   * 并行执行
   */
  private async executeParallel(): Promise<void> {
    const promises: Promise<void>[] = [];

    while ((this.queue.length > 0 || this.running.size > 0) && !this.isCancelled) {
      // 启动新任务
      while (this.queue.length > 0 && this.running.size < this.config.maxParallel) {
        const task = this.findReadyTask();
        if (!task) break;

        this.running.set(task.id, task);
        promises.push(this.executeSubTask(task).then(() => {
          this.running.delete(task.id);
        }));
      }

      if (this.running.size > 0) {
        await Promise.race(promises.filter(p => p));
      }
    }

    await Promise.all(promises);
  }

  /**
   * 查找可执行的任务
   */
  private findReadyTask(): SubTaskDef | null {
    const index = this.queue.findIndex(t => this.checkDependencies(t));
    if (index === -1) return null;
    return this.queue.splice(index, 1)[0];
  }

  /**
   * 检查依赖是否满足
   */
  private checkDependencies(task: SubTaskDef): boolean {
    return task.dependencies.every(depId => {
      const result = this.results.get(depId);
      return result && result.status === 'completed';
    });
  }

  /**
   * 执行单个子任务（带重试机制）
   */
  private async executeSubTask(task: SubTaskDef, retryCount = 0): Promise<SubTaskResult> {
    const result: SubTaskResult = {
      subTaskId: task.id,
      cli: task.assignedCli,
      status: 'running',
      startTime: Date.now(),
    };

    this.emit('taskStart', { task, result, retry: retryCount });

    try {
      const response = await this.executeWithTimeout(task);
      result.response = response;
      result.status = response.error ? 'failed' : 'completed';
      if (response.error) result.error = response.error;
    } catch (error) {
      result.status = 'failed';
      result.error = error instanceof Error ? error.message : String(error);
    }

    result.endTime = Date.now();
    result.duration = result.endTime - result.startTime;

    // 重试逻辑
    if (result.status === 'failed' && retryCount < this.config.retryCount) {
      const shouldRetry = this.shouldRetry(result.error);
      if (shouldRetry) {
        this.emit('taskRetry', { task, result, attempt: retryCount + 1, maxRetries: this.config.retryCount });
        await this.delay(this.getRetryDelay(retryCount));
        return this.executeSubTask(task, retryCount + 1);
      }
    }

    this.results.set(task.id, result);
    this.emit('taskComplete', { task, result, retries: retryCount });

    return result;
  }

  /**
   * 带超时的执行
   */
  private async executeWithTimeout(task: SubTaskDef): Promise<CLIResponse> {
    return new Promise((resolve, reject) => {
      const timeoutId = setTimeout(() => {
        reject(new Error(`任务执行超时 (${this.config.timeout}ms)`));
      }, this.config.timeout);

      this.factory.sendMessage(task.assignedCli, task.description)
        .then(response => {
          clearTimeout(timeoutId);
          resolve(response);
        })
        .catch(error => {
          clearTimeout(timeoutId);
          reject(error);
        });
    });
  }

  /**
   * 判断是否应该重试
   */
  private shouldRetry(error?: string): boolean {
    if (!error) return false;
    // 可重试的错误类型
    const retryableErrors = [
      'timeout', '超时', 'ETIMEDOUT', 'ECONNRESET',
      'rate limit', '限流', 'overloaded', '过载',
      'temporary', '临时', 'retry', '重试',
    ];
    const lowerError = error.toLowerCase();
    return retryableErrors.some(e => lowerError.includes(e.toLowerCase()));
  }

  /**
   * 计算重试延迟（指数退避）
   */
  private getRetryDelay(retryCount: number): number {
    const baseDelay = 1000; // 1秒
    const maxDelay = 30000; // 最大30秒
    const delay = Math.min(baseDelay * Math.pow(2, retryCount), maxDelay);
    // 添加随机抖动
    return delay + Math.random() * 1000;
  }

  /**
   * 延迟函数
   */
  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  /** 取消执行 */
  cancel(): void {
    this.isCancelled = true;
    this.factory.interruptAll();
    // 标记所有运行中的任务为取消状态
    for (const [id, task] of this.running) {
      const result = this.results.get(id);
      if (result && result.status === 'running') {
        result.status = 'cancelled';
        result.endTime = Date.now();
        result.duration = result.endTime - result.startTime;
      }
    }
    this.emit('cancelled', { pendingTasks: this.queue.length, runningTasks: this.running.size });
  }

  /** 获取可恢复的任务 */
  getResumableTasks(): SubTaskDef[] {
    // 返回队列中未执行的任务
    return [...this.queue];
  }

  /** 恢复执行 */
  async resume(): Promise<SubTaskResult[]> {
    if (this.queue.length === 0) {
      return Array.from(this.results.values());
    }

    this.isCancelled = false;
    this.isRunning = true;
    this.emit('resumed', { pendingTasks: this.queue.length });

    // 继续执行剩余任务
    await this.executeSequential();

    this.isRunning = false;
    const results = Array.from(this.results.values());
    this.emit('complete', { results, resumed: true });
    return results;
  }

  /** 获取执行快照（用于持久化） */
  getSnapshot(): ExecutionSnapshot {
    return {
      queue: [...this.queue],
      results: Array.from(this.results.values()),
      isRunning: this.isRunning,
      isCancelled: this.isCancelled,
      timestamp: Date.now(),
    };
  }

  /** 从快照恢复 */
  restoreFromSnapshot(snapshot: ExecutionSnapshot): void {
    this.queue = [...snapshot.queue];
    this.results.clear();
    for (const result of snapshot.results) {
      this.results.set(result.subTaskId, result);
    }
    this.isCancelled = snapshot.isCancelled;
  }

  /** 重置状态 */
  private reset(): void {
    this.queue = [];
    this.running.clear();
    this.results.clear();
    this.isCancelled = false;
  }

  /** 获取执行状态 */
  get status(): { running: boolean; cancelled: boolean; pending: number; completed: number } {
    return {
      running: this.isRunning,
      cancelled: this.isCancelled,
      pending: this.queue.length,
      completed: this.results.size,
    };
  }
}

