/**
 * Worker Pool - Worker 池管理
 *
 * 核心功能：
 * - 管理所有 Worker 实例
 * - 提供 Worker 获取和分配
 * - 监控 Worker 状态
 * - CLI 降级和故障转移
 * - 🆕 任务依赖图调度
 */

import { EventEmitter } from 'events';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { WorkerAgent } from './worker-agent';
import { MessageBus, globalMessageBus } from './message-bus';
import { ExecutionStats, FallbackSuggestion } from './execution-stats';
import { TaskDependencyGraph, DependencyAnalysis, ExecutionBatch } from './task-dependency-graph';
import {
  WorkerType,
  WorkerState,
  WorkerInfo,
  SubTask,
  ExecutionResult,
  BusMessage,
  TaskCompletedMessage,
  TaskFailedMessage,
  ProgressReportMessage,
} from './protocols/types';

/** Worker Pool 配置 */
export interface WorkerPoolConfig {
  cliFactory: CLIAdapterFactory;
  messageBus?: MessageBus;
  orchestratorId?: string;
  /** 执行调度配置 */
  scheduling?: SchedulingConfig;
  /** 执行统计实例（可选，用于 CLI 降级决策） */
  executionStats?: ExecutionStats;
  /** 是否启用 CLI 降级 */
  enableFallback?: boolean;
}

/** 执行调度配置 */
export interface SchedulingConfig {
  /** 最大并行任务数 */
  maxParallel: number;
  /** 任务超时时间 (ms) */
  timeout: number;
  /** 最大重试次数 */
  maxRetries: number;
  /** 重试基础延迟 (ms) */
  retryBaseDelay: number;
}

/** 默认调度配置 */
const DEFAULT_SCHEDULING_CONFIG: SchedulingConfig = {
  maxParallel: 3,
  timeout: 300000, // 5 分钟
  maxRetries: 2,
  retryBaseDelay: 1000,
};

/** 任务执行状态 */
export interface TaskExecutionState {
  subTaskId: string;
  workerType: WorkerType;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled';
  retries: number;
  startTime?: number;
  endTime?: number;
  error?: string;
}

/** Worker 状态变更事件 */
export interface WorkerStateChangeEvent {
  workerId: string;
  workerType: WorkerType;
  oldState: WorkerState;
  newState: WorkerState;
}

/**
 * Worker Pool
 * 管理所有 Worker 实例，提供统一的访问接口
 * 集成 ExecutionScheduler 的高级调度能力
 * 支持 CLI 降级和故障转移
 */
export class WorkerPool extends EventEmitter {
  private workers: Map<WorkerType, WorkerAgent> = new Map();
  private cliFactory: CLIAdapterFactory;
  private messageBus: MessageBus;
  private orchestratorId: string;
  private unsubscribers: Array<() => void> = [];

  // 调度相关
  private schedulingConfig: SchedulingConfig;
  private executionStates: Map<string, TaskExecutionState> = new Map();
  private runningCount: number = 0;

  // 🆕 执行统计和降级相关
  private executionStats?: ExecutionStats;
  private enableFallback: boolean;

  constructor(config: WorkerPoolConfig) {
    super();
    this.cliFactory = config.cliFactory;
    this.messageBus = config.messageBus || globalMessageBus;
    this.orchestratorId = config.orchestratorId || 'orchestrator';
    this.schedulingConfig = { ...DEFAULT_SCHEDULING_CONFIG, ...config.scheduling };

    // 🆕 初始化执行统计和降级配置
    this.executionStats = config.executionStats;
    this.enableFallback = config.enableFallback ?? true;

    this.setupMessageHandlers();
  }

  /** 🆕 设置执行统计实例 */
  setExecutionStats(stats: ExecutionStats): void {
    this.executionStats = stats;
  }

  /** 🆕 获取执行统计实例 */
  getExecutionStats(): ExecutionStats | undefined {
    return this.executionStats;
  }

  /**
   * 初始化所有 Worker
   */
  async initialize(): Promise<void> {
    const workerTypes: WorkerType[] = ['claude', 'codex', 'gemini'];
    
    for (const type of workerTypes) {
      await this.createWorker(type);
    }
    
    console.log(`[WorkerPool] 初始化完成，共 ${this.workers.size} 个 Worker`);
  }

  /**
   * 创建单个 Worker
   */
  private async createWorker(type: WorkerType): Promise<WorkerAgent> {
    if (this.workers.has(type)) {
      return this.workers.get(type)!;
    }

    const worker = new WorkerAgent({
      type,
      cliFactory: this.cliFactory,
      messageBus: this.messageBus,
      orchestratorId: this.orchestratorId,
    });

    // 监听 Worker 状态变更
    worker.on('stateChange', (newState: WorkerState) => {
      this.emit('workerStateChange', {
        workerId: worker.id,
        workerType: type,
        newState,
        source: 'worker',  // 标识消息来源
      });
    });

    // 监听 Worker 输出（标识来源为 worker）
    worker.on('output', (chunk: string) => {
      this.emit('workerOutput', {
        workerId: worker.id,
        workerType: type,
        chunk,
        source: 'worker',  // 标识消息来源
      });
    });

    this.workers.set(type, worker);
    console.log(`[WorkerPool] 创建 Worker: ${worker.id}`);
    
    return worker;
  }

  /**
   * 设置消息处理器
   */
  private setupMessageHandlers(): void {
    // 监听任务完成消息
    const unsubCompleted = this.messageBus.subscribe('task_completed', (msg) => {
      const message = msg as TaskCompletedMessage;
      this.emit('taskCompleted', message.payload.result);
    });
    this.unsubscribers.push(unsubCompleted);

    // 监听任务失败消息
    const unsubFailed = this.messageBus.subscribe('task_failed', (msg) => {
      const message = msg as TaskFailedMessage;
      this.emit('taskFailed', message.payload);
    });
    this.unsubscribers.push(unsubFailed);

    // 监听进度汇报消息
    const unsubProgress = this.messageBus.subscribe('progress_report', (msg) => {
      const message = msg as ProgressReportMessage;
      this.emit('workerProgress', message.payload);
    });
    this.unsubscribers.push(unsubProgress);
  }

  /**
   * 获取指定类型的 Worker
   */
  getWorker(type: WorkerType): WorkerAgent | undefined {
    return this.workers.get(type);
  }

  /**
   * 获取或创建 Worker
   */
  async getOrCreateWorker(type: WorkerType): Promise<WorkerAgent> {
    const existing = this.workers.get(type);
    if (existing) {
      return existing;
    }
    return this.createWorker(type);
  }

  /**
   * 获取所有 Worker
   */
  getAllWorkers(): WorkerAgent[] {
    return Array.from(this.workers.values());
  }

  /**
   * 获取所有 Worker 信息
   */
  getAllWorkerInfo(): WorkerInfo[] {
    return this.getAllWorkers().map(w => w.info);
  }

  /**
   * 获取空闲的 Worker
   */
  getIdleWorkers(): WorkerAgent[] {
    return this.getAllWorkers().filter(w => w.state === 'idle');
  }

  /**
   * 获取指定类型的空闲 Worker
   */
  getIdleWorker(type: WorkerType): WorkerAgent | undefined {
    const worker = this.workers.get(type);
    return worker?.state === 'idle' ? worker : undefined;
  }

  /**
   * 检查指定类型的 Worker 是否空闲
   */
  isWorkerIdle(type: WorkerType): boolean {
    const worker = this.workers.get(type);
    return (worker?.state === 'idle') || false;
  }

  /**
   * 分发任务给指定 Worker
   */
  async dispatchTask(
    type: WorkerType,
    taskId: string,
    subTask: SubTask,
    context?: string
  ): Promise<ExecutionResult> {
    const worker = await this.getOrCreateWorker(type);

    if (worker.state !== 'idle') {
      throw new Error(`Worker ${type} 当前状态为 ${worker.state}，无法接受新任务`);
    }

    return worker.executeTask(taskId, subTask, context);
  }

  /**
   * 带重试、降级和超时的任务分发（集成 ExecutionScheduler 能力）
   * 🆕 支持 CLI 降级：当原 CLI 失败时，自动尝试其他可用 CLI
   */
  async dispatchTaskWithRetry(
    type: WorkerType,
    taskId: string,
    subTask: SubTask,
    context?: string
  ): Promise<ExecutionResult> {
    const state: TaskExecutionState = {
      subTaskId: subTask.id,
      workerType: type,
      status: 'pending',
      retries: 0,
    };
    this.executionStates.set(subTask.id, state);

    let lastError: Error | null = null;
    let currentCli: WorkerType = type;
    const triedClis: WorkerType[] = [];

    for (let attempt = 0; attempt <= this.schedulingConfig.maxRetries; attempt++) {
      state.retries = attempt;
      state.status = 'running';
      state.workerType = currentCli;
      const startTime = Date.now();
      state.startTime = startTime;

      try {
        const result = await this.executeWithTimeout(currentCli, taskId, subTask, context);
        const duration = Date.now() - startTime;

        // 🆕 记录执行统计
        this.recordExecution(currentCli, taskId, subTask.id, result.success, duration, result.error);

        state.status = result.success ? 'completed' : 'failed';
        state.endTime = Date.now();

        if (result.success) {
          this.runningCount--;
          return result;
        }

        // 检查是否应该重试或降级
        if (!this.shouldRetry(result.error) || attempt >= this.schedulingConfig.maxRetries) {
          // 🆕 尝试降级到其他 CLI
          triedClis.push(currentCli);
          const fallback = this.tryFallback(currentCli, triedClis);
          if (fallback) {
            currentCli = fallback.suggestedCli;
            console.log(`[WorkerPool] CLI 降级: ${type} -> ${currentCli}，原因: ${fallback.reason}`);
            this.emit('cliFallback', { original: type, fallback: currentCli, reason: fallback.reason });
            continue; // 使用新 CLI 重试
          }

          state.error = result.error;
          this.runningCount--;
          return result;
        }

        lastError = new Error(result.error || 'Unknown error');

      } catch (error) {
        const duration = Date.now() - startTime;
        lastError = error instanceof Error ? error : new Error(String(error));
        state.error = lastError.message;

        // 🆕 记录失败统计
        this.recordExecution(currentCli, taskId, subTask.id, false, duration, lastError.message);

        if (!this.shouldRetry(lastError.message) || attempt >= this.schedulingConfig.maxRetries) {
          // 🆕 尝试降级到其他 CLI
          triedClis.push(currentCli);
          const fallback = this.tryFallback(currentCli, triedClis);
          if (fallback) {
            currentCli = fallback.suggestedCli;
            console.log(`[WorkerPool] CLI 降级: ${type} -> ${currentCli}，原因: ${fallback.reason}`);
            this.emit('cliFallback', { original: type, fallback: currentCli, reason: fallback.reason });
            continue; // 使用新 CLI 重试
          }

          state.status = 'failed';
          state.endTime = Date.now();
          this.runningCount--;
          throw lastError;
        }
      }

      // 重试延迟（指数退避）
      const delay = this.getRetryDelay(attempt);
      console.log(`[WorkerPool] 任务 ${subTask.id} 重试 ${attempt + 1}/${this.schedulingConfig.maxRetries}，延迟 ${delay}ms`);
      this.emit('taskRetry', { subTaskId: subTask.id, attempt: attempt + 1, delay });
      await this.delay(delay);
    }

    state.status = 'failed';
    state.endTime = Date.now();
    this.runningCount--;
    throw lastError || new Error('任务执行失败');
  }

  /**
   * 带超时的任务执行
   */
  private async executeWithTimeout(
    type: WorkerType,
    taskId: string,
    subTask: SubTask,
    context?: string
  ): Promise<ExecutionResult> {
    this.runningCount++;

    return new Promise(async (resolve, reject) => {
      const timeoutId = setTimeout(() => {
        reject(new Error(`任务执行超时 (${this.schedulingConfig.timeout}ms)`));
      }, this.schedulingConfig.timeout);

      try {
        const result = await this.dispatchTask(type, taskId, subTask, context);
        clearTimeout(timeoutId);
        resolve(result);
      } catch (error) {
        clearTimeout(timeoutId);
        reject(error);
      }
    });
  }

  /**
   * 判断是否应该重试
   */
  private shouldRetry(error?: string): boolean {
    if (!error) return false;
    const retryablePatterns = [
      'timeout', '超时', 'ETIMEDOUT', 'ECONNRESET',
      'rate limit', '限流', 'overloaded', '过载',
      'temporary', '临时', 'retry', '重试',
      'network', '网络',
    ];
    const lowerError = error.toLowerCase();
    return retryablePatterns.some(p => lowerError.includes(p.toLowerCase()));
  }

  /**
   * 计算重试延迟（指数退避 + 随机抖动）
   */
  private getRetryDelay(attempt: number): number {
    const baseDelay = this.schedulingConfig.retryBaseDelay;
    const maxDelay = 30000;
    const delay = Math.min(baseDelay * Math.pow(2, attempt), maxDelay);
    return delay + Math.random() * 1000;
  }

  /**
   * 延迟函数
   */
  private delay(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  /**
   * 🆕 记录执行统计
   */
  private recordExecution(
    cli: WorkerType,
    taskId: string,
    subTaskId: string,
    success: boolean,
    duration: number,
    error?: string
  ): void {
    if (this.executionStats) {
      this.executionStats.recordExecution({
        cli,
        taskId,
        subTaskId,
        success,
        duration,
        error,
      });
    }
  }

  /**
   * 🆕 尝试 CLI 降级
   * @param failedCli 失败的 CLI
   * @param triedClis 已尝试过的 CLI 列表
   * @returns 降级建议，如果没有可用的降级选项则返回 null
   */
  private tryFallback(failedCli: WorkerType, triedClis: WorkerType[]): FallbackSuggestion | null {
    if (!this.enableFallback) {
      return null;
    }

    // 如果有执行统计，使用智能降级
    if (this.executionStats) {
      return this.executionStats.getFallbackSuggestion(failedCli, triedClis);
    }

    // 没有统计数据时，使用简单的轮换策略
    const allClis: WorkerType[] = ['claude', 'codex', 'gemini'];
    const availableClis = allClis.filter(cli => !triedClis.includes(cli));

    if (availableClis.length === 0) {
      return null;
    }

    // 简单选择第一个可用的 CLI
    const suggestedCli = availableClis[0];
    return {
      originalCli: failedCli,
      suggestedCli,
      reason: `${failedCli} 执行失败，尝试使用 ${suggestedCli}`,
      confidence: 0.5,
    };
  }

  /**
   * 获取任务执行状态
   */
  getExecutionState(subTaskId: string): TaskExecutionState | undefined {
    return this.executionStates.get(subTaskId);
  }

  /**
   * 获取当前运行中的任务数
   */
  getRunningCount(): number {
    return this.runningCount;
  }

  /**
   * 清除执行状态
   */
  clearExecutionStates(): void {
    this.executionStates.clear();
    this.runningCount = 0;
  }

  /**
   * 通过消息总线分发任务（异步）
   */
  dispatchTaskAsync(
    type: WorkerType,
    taskId: string,
    subTask: SubTask,
    context?: string
  ): void {
    const worker = this.workers.get(type);
    if (!worker) {
      console.error(`[WorkerPool] Worker ${type} 不存在`);
      return;
    }

    this.messageBus.dispatchTask(
      this.orchestratorId,
      worker.id,
      taskId,
      subTask,
      context
    );
  }

  /**
   * 取消指定 Worker 的任务
   */
  async cancelWorkerTask(type: WorkerType): Promise<void> {
    const worker = this.workers.get(type);
    if (worker) {
      await worker.cancel();
    }
  }

  /**
   * 取消所有 Worker 的任务
   */
  async cancelAllTasks(): Promise<void> {
    const promises = this.getAllWorkers().map(w => w.cancel());
    await Promise.all(promises);
  }

  /**
   * 广播取消命令
   */
  broadcastCancel(): void {
    this.messageBus.broadcastCommand(this.orchestratorId, 'cancel_all');
  }

  /**
   * 🆕 基于依赖图执行任务批次
   * 按照拓扑排序的批次顺序执行任务，同一批次内的任务并行执行
   */
  async executeWithDependencyGraph(
    taskId: string,
    subTasks: SubTask[],
    context?: string
  ): Promise<ExecutionResult[]> {
    // 构建依赖图
    const graph = new TaskDependencyGraph();

    // 添加所有任务到图中
    for (const subTask of subTasks) {
      graph.addTask(subTask.id, subTask.title || subTask.description, subTask);
    }

    // 添加依赖关系
    for (const subTask of subTasks) {
      if (subTask.dependencies && subTask.dependencies.length > 0) {
        graph.addDependencies(subTask.id, subTask.dependencies);
      }
    }

    // 分析依赖图
    const analysis = graph.analyze();

    if (analysis.hasCycle) {
      console.error('[WorkerPool] 检测到循环依赖:', analysis.cycleNodes);
      throw new Error(`任务存在循环依赖: ${analysis.cycleNodes?.join(', ')}`);
    }

    console.log(`[WorkerPool] 依赖图分析完成:`);
    console.log(`  - 任务总数: ${subTasks.length}`);
    console.log(`  - 执行批次: ${analysis.executionBatches.length}`);
    console.log(`  - 关键路径: ${analysis.criticalPath.join(' -> ')}`);

    // 发出依赖图分析事件
    this.emit('dependencyAnalysis', {
      taskId,
      analysis,
      mermaid: graph.toMermaid(),
    });

    // 按批次执行任务
    const allResults: ExecutionResult[] = [];

    for (const batch of analysis.executionBatches) {
      console.log(`[WorkerPool] 执行批次 ${batch.batchIndex + 1}/${analysis.executionBatches.length}:`,
        batch.taskIds.join(', '));

      // 获取批次中的任务
      const batchTasks = batch.taskIds
        .map(id => graph.getTask(id)?.data as SubTask)
        .filter((t): t is SubTask => t !== undefined);

      // 并行执行批次内的任务
      const batchResults = await this.executeBatchParallel(taskId, batchTasks, context);
      allResults.push(...batchResults);

      // 更新图中的任务状态
      for (const result of batchResults) {
        graph.updateTaskStatus(
          result.subTaskId,
          result.success ? 'completed' : 'failed'
        );
      }

      // 如果有任务失败，检查是否影响后续任务
      const failedTasks = batchResults.filter(r => !r.success);
      if (failedTasks.length > 0) {
        console.warn(`[WorkerPool] 批次 ${batch.batchIndex + 1} 有 ${failedTasks.length} 个任务失败`);
        // 继续执行，让后续批次中不依赖失败任务的任务继续执行
      }
    }

    return allResults;
  }

  /**
   * 🆕 并行执行一批任务
   */
  private async executeBatchParallel(
    taskId: string,
    subTasks: SubTask[],
    context?: string
  ): Promise<ExecutionResult[]> {
    // 限制并行数量
    const maxParallel = this.schedulingConfig.maxParallel;
    const results: ExecutionResult[] = [];

    // 分组执行
    for (let i = 0; i < subTasks.length; i += maxParallel) {
      const chunk = subTasks.slice(i, i + maxParallel);
      const chunkPromises = chunk.map(subTask =>
        this.dispatchTaskWithRetry(
          subTask.assignedWorker || subTask.assignedCli || 'claude',
          taskId,
          subTask,
          context
        ).catch(error => ({
          workerId: 'unknown',
          workerType: subTask.assignedWorker || 'claude',
          taskId,
          subTaskId: subTask.id,
          result: '',
          success: false,
          duration: 0,
          error: error instanceof Error ? error.message : String(error),
        } as ExecutionResult))
      );

      const chunkResults = await Promise.all(chunkPromises);
      results.push(...chunkResults);
    }

    return results;
  }

  /**
   * 🆕 创建任务依赖图（供外部使用）
   */
  createDependencyGraph(subTasks: SubTask[]): TaskDependencyGraph {
    const graph = new TaskDependencyGraph();

    for (const subTask of subTasks) {
      graph.addTask(subTask.id, subTask.title || subTask.description, subTask);
    }

    for (const subTask of subTasks) {
      if (subTask.dependencies && subTask.dependencies.length > 0) {
        graph.addDependencies(subTask.id, subTask.dependencies);
      }
    }

    return graph;
  }

  /**
   * 销毁 Worker Pool
   */
  dispose(): void {
    // 取消所有订阅
    this.unsubscribers.forEach(unsub => unsub());
    this.unsubscribers = [];

    // 销毁所有 Worker
    this.workers.forEach(worker => worker.dispose());
    this.workers.clear();

    this.removeAllListeners();
    console.log('[WorkerPool] 已销毁');
  }
}