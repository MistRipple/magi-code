/**
 * DispatchBatch - dispatch_task 生命周期追踪器
 *
 * 管理一次 orchestrator LLM 调用中发出的所有 dispatch_task 的状态、
 * 依赖关系和生命周期，为 Phase C 汇总和依赖链编排提供基础设施。
 *
 * 生命周期：创建 → 注册任务 → 追加任务 → 完成检测 → 汇总触发 → 归档
 */

import { EventEmitter } from 'events';
import type { WorkerSlot } from '../../types';
import { logger, LogCategory } from '../../logging';

// ============================================================================
// CancellationToken
// ============================================================================

/**
 * CancellationToken — 取消信号传递对象
 *
 * 封装 AbortController，在 DispatchBatch → Worker → LLM 请求之间
 * 建立统一的取消信号链。
 */
export class CancellationToken {
  private readonly controller = new AbortController();
  private _reason?: string;
  private readonly callbacks: Array<(reason: string) => void> = [];

  get isCancelled(): boolean {
    return this.controller.signal.aborted;
  }

  get reason(): string | undefined {
    return this._reason;
  }

  /** 暴露原始 AbortSignal，供 LLM 请求直接使用 */
  get signal(): AbortSignal {
    return this.controller.signal;
  }

  cancel(reason: string = '用户取消'): void {
    if (this.isCancelled) return;
    this._reason = reason;
    this.controller.abort(reason);
    for (const cb of this.callbacks) {
      try { cb(reason); } catch { /* 回调异常不阻塞取消链 */ }
    }
  }

  /** 注册取消回调 */
  onCancel(callback: (reason: string) => void): void {
    if (this.isCancelled) {
      callback(this._reason || '已取消');
      return;
    }
    this.callbacks.push(callback);
  }

  /** 如果已取消则抛异常，用于循环入口快速退出 */
  throwIfCancelled(): void {
    if (this.isCancelled) {
      throw new CancellationError(this._reason || '任务被取消');
    }
  }
}

export class CancellationError extends Error {
  readonly isCancellation = true;
  constructor(message: string) {
    super(message);
    this.name = 'CancellationError';
  }
}

// ============================================================================
// 类型定义
// ============================================================================

/** dispatch_task 状态 */
export type DispatchStatus = 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';

/** 单个 dispatch 条目 */
export interface DispatchEntry {
  taskId: string;
  worker: WorkerSlot;
  task: string;
  files: string[];
  dependsOn: string[];
  status: DispatchStatus;
  result?: DispatchResult;
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
}

/** dispatch 执行结果 */
export interface DispatchResult {
  success: boolean;
  summary: string;
  modifiedFiles?: string[];
  errors?: string[];
  /** 任务消耗的 token 统计 */
  tokenUsage?: { inputTokens: number; outputTokens: number };
}

/** Token 消耗统计 */
export interface TokenConsumption {
  /** 已消耗的输入 token */
  inputTokens: number;
  /** 已消耗的输出 token */
  outputTokens: number;
  /** 总消耗 */
  totalTokens: number;
}

/** DispatchBatch 阶段（显式状态机） */
export type BatchPhase = 'active' | 'summarizing' | 'archived';

/** 合法的阶段转换路径 */
const ALLOWED_PHASE_TRANSITIONS: Record<BatchPhase, BatchPhase[]> = {
  active: ['summarizing', 'archived'],     // 正常完成 → summarizing，取消 → archived
  summarizing: ['archived'],               // Phase C 完成后归档
  archived: [],                            // 终态，不可转换
};

/** DispatchBatch 事件 */
export interface DispatchBatchEvents {
  /** 单个任务状态变化 */
  'task:statusChanged': (taskId: string, status: DispatchStatus, result?: DispatchResult) => void;
  /** 所有任务完成（进入 summarizing 阶段，触发 Phase C） */
  'batch:allCompleted': (batchId: string, entries: DispatchEntry[]) => void;
  /** 阶段转换 */
  'phase:changed': (batchId: string, phase: BatchPhase) => void;
  /** 任务就绪可执行（依赖已满足） */
  'task:ready': (taskId: string, entry: DispatchEntry) => void;
  /** Batch 被取消 */
  'batch:cancelled': (batchId: string, reason: string, entries: DispatchEntry[]) => void;
}

// ============================================================================
// DispatchBatch 实现
// ============================================================================

export class DispatchBatch extends EventEmitter {
  readonly id: string;
  private entries: Map<string, DispatchEntry> = new Map();
  private _phase: BatchPhase = 'active';
  private readonly createdAt: number;
  /** 触发本 Batch 的用户原始请求（Phase C 汇总需要） */
  userPrompt: string = '';
  /** 取消信号 Token，整个 Batch 共享 */
  readonly cancellationToken = new CancellationToken();
  /** 累计 Token 消耗 */
  private _tokenConsumption = { inputTokens: 0, outputTokens: 0 };
  /** 归档等待队列：等待 batch 归档（Worker 执行 + Phase C 完成）的 Promise resolve 函数 */
  private archiveResolvers: Array<() => void> = [];
  /** 最后活动时间戳：任何 Worker 状态变化都会刷新 */
  private _lastActivityAt: number;

  constructor(batchId?: string) {
    super();
    this.id = batchId || `batch-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`;
    this.createdAt = Date.now();
    this._lastActivityAt = Date.now();
  }

  get status(): BatchPhase {
    return this._phase;
  }

  get phase(): BatchPhase {
    return this._phase;
  }

  get size(): number {
    return this.entries.size;
  }

  /**
   * 显式阶段转换（状态机）
   *
   * 合法路径: active → summarizing → archived
   *           active → archived（取消场景）
   */
  transitionTo(next: BatchPhase): void {
    if (this._phase === next) return;
    const allowed = ALLOWED_PHASE_TRANSITIONS[this._phase];
    if (!allowed.includes(next)) {
      throw new Error(`DispatchBatch 非法阶段转换: ${this._phase} → ${next} (batch: ${this.id})`);
    }
    const prev = this._phase;
    this._phase = next;
    this.emit('phase:changed', this.id, next);
    logger.info('DispatchBatch.阶段转换', {
      batchId: this.id, from: prev, to: next,
    }, LogCategory.ORCHESTRATOR);
  }

  /**
   * 注册新的 dispatch 任务
   */
  register(params: {
    taskId: string;
    worker: WorkerSlot;
    task: string;
    files?: string[];
    dependsOn?: string[];
  }): DispatchEntry {
    if (this._phase === 'archived') {
      throw new Error(`DispatchBatch ${this.id} 已归档，无法注册新任务`);
    }

    if (this.entries.has(params.taskId)) {
      throw new Error(`任务 ${params.taskId} 已存在于 Batch ${this.id}`);
    }

    // 验证 dependsOn 引用的 taskId 是否已注册（允许后续注册的任务引用先注册的）
    const dependsOn = params.dependsOn || [];
    for (const depId of dependsOn) {
      if (depId === params.taskId) {
        throw new Error(`任务 ${params.taskId} 不能依赖自身`);
      }
    }

    const entry: DispatchEntry = {
      taskId: params.taskId,
      worker: params.worker,
      task: params.task,
      files: params.files || [],
      dependsOn,
      status: dependsOn.length > 0 ? 'waiting_deps' : 'pending',
      createdAt: Date.now(),
    };

    this.entries.set(params.taskId, entry);

    logger.debug('DispatchBatch.注册', {
      batchId: this.id,
      taskId: params.taskId,
      worker: params.worker,
      dependsOn,
      status: entry.status,
    }, LogCategory.ORCHESTRATOR);

    return entry;
  }

  /**
   * 刷新活动时间戳
   *
   * 供 Worker 上报进度、LLM chunk 等活动信号调用，
   * 让 waitForArchive 的 idle 超时检测知道 batch 仍在活跃工作。
   */
  touchActivity(): void {
    this._lastActivityAt = Date.now();
  }

  /**
   * 更新任务状态
   */
  updateStatus(taskId: string, status: DispatchStatus, result?: DispatchResult): void {
    const entry = this.entries.get(taskId);
    if (!entry) {
      logger.warn('DispatchBatch.更新状态.任务不存在', { batchId: this.id, taskId }, LogCategory.ORCHESTRATOR);
      return;
    }

    this._lastActivityAt = Date.now();
    entry.status = status;
    if (status === 'running' && !entry.startedAt) {
      entry.startedAt = Date.now();
    }
    if (status === 'completed' || status === 'failed' || status === 'skipped' || status === 'cancelled') {
      entry.completedAt = Date.now();
      if (result) {
        entry.result = result;
        // 累计 token 消耗
        if (result.tokenUsage) {
          this._tokenConsumption.inputTokens += result.tokenUsage.inputTokens;
          this._tokenConsumption.outputTokens += result.tokenUsage.outputTokens;
        }
      }
    }

    this.emit('task:statusChanged', taskId, status, result);

    // 任务完成后检查是否有依赖它的后续任务可以执行
    if (status === 'completed' || status === 'failed' || status === 'skipped' || status === 'cancelled') {
      this.checkDependents(taskId);
      this.checkAllCompleted();
    }
  }

  /**
   * 标记任务开始运行
   */
  markRunning(taskId: string): void {
    this.updateStatus(taskId, 'running');
  }

  /**
   * 标记任务完成
   */
  markCompleted(taskId: string, result: DispatchResult): void {
    this.updateStatus(taskId, 'completed', result);
  }

  /**
   * 标记任务失败
   */
  markFailed(taskId: string, result: DispatchResult): void {
    this.updateStatus(taskId, 'failed', result);
  }

  /**
   * 检查任务是否可以执行（所有依赖已完成）
   */
  canExecute(taskId: string): boolean {
    const entry = this.entries.get(taskId);
    if (!entry) return false;
    if (entry.dependsOn.length === 0) return true;

    return entry.dependsOn.every(depId => {
      const dep = this.entries.get(depId);
      return dep && dep.status === 'completed';
    });
  }

  /**
   * 检查是否有依赖失败（需要级联跳过）
   */
  hasDependencyFailed(taskId: string): boolean {
    const entry = this.entries.get(taskId);
    if (!entry) return false;

    return entry.dependsOn.some(depId => {
      const dep = this.entries.get(depId);
      return dep && (dep.status === 'failed' || dep.status === 'skipped' || dep.status === 'cancelled');
    });
  }

  /**
   * 检查所有任务是否完成
   */
  isAllCompleted(): boolean {
    if (this.entries.size === 0) return false;
    return Array.from(this.entries.values()).every(
      e => e.status === 'completed' || e.status === 'failed' || e.status === 'skipped' || e.status === 'cancelled'
    );
  }

  /**
   * 检测文件冲突（多个并行任务操作相同文件）
   * 返回冲突组：每组包含操作同一文件的 taskId 列表
   */
  detectFileConflicts(): Map<string, string[]> {
    const fileToTasks = new Map<string, string[]>();

    for (const entry of this.entries.values()) {
      // 只检测可能并行的任务（无依赖关系的）
      for (const file of entry.files) {
        const tasks = fileToTasks.get(file) || [];
        tasks.push(entry.taskId);
        fileToTasks.set(file, tasks);
      }
    }

    // 过滤出冲突文件（有多个任务操作且这些任务间无依赖关系）
    const conflicts = new Map<string, string[]>();
    for (const [file, taskIds] of fileToTasks) {
      if (taskIds.length <= 1) continue;

      // 检查这些任务间是否有依赖关系
      const independentTasks = taskIds.filter(taskId => {
        const entry = this.entries.get(taskId)!;
        return !taskIds.some(otherId =>
          otherId !== taskId && entry.dependsOn.includes(otherId)
        );
      });

      if (independentTasks.length > 1) {
        conflicts.set(file, independentTasks);
      }
    }

    return conflicts;
  }

  /**
   * 解决文件冲突 — 将冲突的并行任务自动转为串行
   *
   * 约束 6：多个并行 Worker 声明了重叠的 files 参数时，
   * 自动为后注册的冲突任务添加 depends_on，使其串行执行。
   *
   * @returns 实际添加的依赖数量（0 表示无冲突或已有依赖）
   */
  resolveFileConflicts(): number {
    const conflicts = this.detectFileConflicts();
    if (conflicts.size === 0) return 0;

    let addedDeps = 0;

    // 收集所有存在冲突的 taskId 对（去重）
    const processedPairs = new Set<string>();

    for (const [_file, conflictIds] of conflicts) {
      // 按注册顺序排列（createdAt 时间戳）
      const sorted = conflictIds
        .map(id => this.entries.get(id)!)
        .sort((a, b) => a.createdAt - b.createdAt);

      // 将后注册的任务依赖于前一个任务（链式串行）
      for (let i = 1; i < sorted.length; i++) {
        const prev = sorted[i - 1];
        const curr = sorted[i];
        const pairKey = `${prev.taskId}->${curr.taskId}`;

        if (processedPairs.has(pairKey)) continue;
        processedPairs.add(pairKey);

        // 如果已有依赖关系则跳过
        if (curr.dependsOn.includes(prev.taskId)) continue;

        curr.dependsOn.push(prev.taskId);
        // 如果当前任务原本是 pending，改为 waiting_deps
        if (curr.status === 'pending') {
          curr.status = 'waiting_deps';
        }
        addedDeps++;

        logger.info('DispatchBatch.文件冲突.自动串行化', {
          batchId: this.id,
          file: _file,
          predecessor: prev.taskId,
          dependent: curr.taskId,
        }, LogCategory.ORCHESTRATOR);
      }
    }

    return addedDeps;
  }

  /**
   * 环检测 + 拓扑排序
   * 返回排序后的 taskId 列表，如有环则抛异常
   */
  topologicalSort(): string[] {
    const inDegree = new Map<string, number>();
    const adjList = new Map<string, string[]>();

    // 初始化
    for (const [taskId, entry] of this.entries) {
      inDegree.set(taskId, entry.dependsOn.length);
      for (const depId of entry.dependsOn) {
        const dependents = adjList.get(depId) || [];
        dependents.push(taskId);
        adjList.set(depId, dependents);
      }
    }

    // BFS（Kahn 算法）
    const queue: string[] = [];
    for (const [taskId, degree] of inDegree) {
      if (degree === 0) queue.push(taskId);
    }

    const sorted: string[] = [];
    while (queue.length > 0) {
      const current = queue.shift()!;
      sorted.push(current);

      for (const dependent of (adjList.get(current) || [])) {
        const newDegree = (inDegree.get(dependent) || 1) - 1;
        inDegree.set(dependent, newDegree);
        if (newDegree === 0) queue.push(dependent);
      }
    }

    if (sorted.length !== this.entries.size) {
      const remaining = Array.from(this.entries.keys()).filter(id => !sorted.includes(id));
      throw new Error(`依赖链存在环形引用: ${remaining.join(', ')}`);
    }

    return sorted;
  }

  /**
   * 验证依赖链深度（上限 5 层）
   */
  validateDepthLimit(maxDepth: number = 5): void {
    for (const taskId of this.entries.keys()) {
      const depth = this.calculateDepth(taskId, new Set());
      if (depth > maxDepth) {
        throw new Error(`任务 ${taskId} 的依赖链深度 ${depth} 超过上限 ${maxDepth}`);
      }
    }
  }

  /**
   * 获取指定任务条目
   */
  getEntry(taskId: string): DispatchEntry | undefined {
    return this.entries.get(taskId);
  }

  /**
   * 获取所有条目
   */
  getEntries(): DispatchEntry[] {
    return Array.from(this.entries.values());
  }

  /**
   * 获取就绪可执行的任务列表
   */
  getReadyTasks(): DispatchEntry[] {
    return this.getEntries().filter(e =>
      (e.status === 'pending' || e.status === 'waiting_deps') && this.canExecute(e.taskId)
    );
  }

  /**
   * 获取就绪任务（Worker 隔离模式）
   *
   * 同类型 Worker 串行 + 不同类型并行：
   * 每个 WorkerSlot 最多返回 1 个 ready 任务（且该 Worker 无 running 任务）
   */
  getReadyTasksIsolated(): DispatchEntry[] {
    const allReady = this.getReadyTasks();
    const runningWorkers = new Set<WorkerSlot>();

    // 收集当前有任务在运行的 Worker 类型
    for (const entry of this.entries.values()) {
      if (entry.status === 'running') {
        runningWorkers.add(entry.worker);
      }
    }

    // 每个 WorkerSlot 最多挑选 1 个（且该 Worker 没有 running 任务）
    const selectedWorkers = new Set<WorkerSlot>();
    const result: DispatchEntry[] = [];

    for (const entry of allReady) {
      if (runningWorkers.has(entry.worker)) continue;
      if (selectedWorkers.has(entry.worker)) continue;
      selectedWorkers.add(entry.worker);
      result.push(entry);
    }

    return result;
  }

  /**
   * 获取汇总统计
   */
  getSummary(): {
    total: number;
    completed: number;
    failed: number;
    skipped: number;
    cancelled: number;
    running: number;
    pending: number;
  } {
    const entries = this.getEntries();
    return {
      total: entries.length,
      completed: entries.filter(e => e.status === 'completed').length,
      failed: entries.filter(e => e.status === 'failed').length,
      skipped: entries.filter(e => e.status === 'skipped').length,
      cancelled: entries.filter(e => e.status === 'cancelled').length,
      running: entries.filter(e => e.status === 'running').length,
      pending: entries.filter(e => e.status === 'pending' || e.status === 'waiting_deps').length,
    };
  }

  /**
   * 取消整个 Batch 中所有未完成的任务
   *
   * 信号传递链：cancelAll → CancellationToken.cancel → Worker 检测到取消 → LLM 请求中断
   * 已完成的任务不受影响，不触发 Phase C。
   */
  cancelAll(reason: string = '用户取消'): void {
    if (this._phase !== 'active') return;

    // 发出取消信号
    this.cancellationToken.cancel(reason);

    // 将所有未完成的任务标记为 cancelled
    for (const entry of this.entries.values()) {
      if (entry.status === 'pending' || entry.status === 'waiting_deps' || entry.status === 'running') {
        entry.status = 'cancelled';
        entry.completedAt = Date.now();
        entry.result = { success: false, summary: `取消: ${reason}` };
        this.emit('task:statusChanged', entry.taskId, 'cancelled', entry.result);
      }
    }

    this.transitionTo('archived');
    // 释放所有等待归档的 Promise（与 archive() 对齐）
    for (const resolve of this.archiveResolvers) resolve();
    this.archiveResolvers = [];
    this.emit('batch:cancelled', this.id, reason, this.getEntries());

    logger.info('DispatchBatch.取消', {
      batchId: this.id,
      reason,
      summary: this.getSummary(),
    }, LogCategory.ORCHESTRATOR);
  }

  /**
   * 归档 Batch
   */
  archive(): void {
    this.transitionTo('archived');
    // 释放所有等待归档的 Promise
    for (const resolve of this.archiveResolvers) resolve();
    this.archiveResolvers = [];
    logger.info('DispatchBatch.归档', {
      batchId: this.id,
      summary: this.getSummary(),
      tokenConsumption: this.getTokenConsumption(),
    }, LogCategory.ORCHESTRATOR);
  }

  /**
   * 等待 Batch 归档（所有 Worker 完成 + Phase C 汇总完成）
   *
   * 用于 MissionDrivenEngine.execute() 在 sendMessage 返回后同步等待
   * dispatch 链路真正结束，确保 TASK_COMPLETED 在正确时机发出。
   *
   * 超时策略（idle 模式）：
   * - 每 30 秒检查一次最后活动时间戳
   * - 如果距离最后活动超过 idleTimeoutMs（默认 5 分钟），判定为阻断
   * - 正常工作中的 Worker 会通过 updateStatus / touchActivity 持续刷新时间戳
   * - 超时后自动 cancelAll 并归档，防止永久阻塞
   */
  waitForArchive(idleTimeoutMs: number = 5 * 60 * 1000): Promise<void> {
    if (this._phase === 'archived') return Promise.resolve();
    return new Promise<void>(resolve => {
      const CHECK_INTERVAL = 30_000; // 30 秒检查一次
      const checker = setInterval(() => {
        if (this._phase === 'archived') {
          clearInterval(checker);
          return;
        }
        const idleTime = Date.now() - this._lastActivityAt;
        if (idleTime > idleTimeoutMs) {
          clearInterval(checker);
          logger.warn('DispatchBatch.waitForArchive.idle超时', {
            batchId: this.id,
            idleTimeMs: idleTime,
            idleTimeoutMs,
            summary: this.getSummary(),
          }, LogCategory.ORCHESTRATOR);
          this.cancelAll(`无活动超时 (idle ${Math.round(idleTime / 1000)}s)`);
        }
      }, CHECK_INTERVAL);

      this.archiveResolvers.push(() => {
        clearInterval(checker);
        resolve();
      });
    });
  }

  // ============================================================================
  // 成本控制
  // ============================================================================

  /**
   * 获取当前 Token 消耗统计
   */
  getTokenConsumption(): TokenConsumption {
    const total = this._tokenConsumption.inputTokens + this._tokenConsumption.outputTokens;
    return {
      inputTokens: this._tokenConsumption.inputTokens,
      outputTokens: this._tokenConsumption.outputTokens,
      totalTokens: total,
    };
  }

  // ============================================================================
  // 私有方法
  // ============================================================================

  /**
   * 检查依赖于指定任务的后续任务是否可以执行
   */
  private checkDependents(completedTaskId: string): void {
    const completedEntry = this.entries.get(completedTaskId);

    for (const [taskId, entry] of this.entries) {
      if (!entry.dependsOn.includes(completedTaskId)) continue;
      if (entry.status !== 'waiting_deps') continue;

      // 前序任务失败 → 级联跳过
      if (completedEntry && (completedEntry.status === 'failed' || completedEntry.status === 'skipped' || completedEntry.status === 'cancelled')) {
        this.updateStatus(taskId, 'skipped', {
          success: false,
          summary: `前序任务 ${completedTaskId} 失败/跳过，级联跳过`,
        });
        continue;
      }

      // 检查所有依赖是否就绪
      if (this.canExecute(taskId)) {
        entry.status = 'pending';
        this.emit('task:ready', taskId, entry);
      }
    }
  }

  /**
   * 检查是否所有任务完成
   */
  private checkAllCompleted(): void {
    if (this.isAllCompleted() && this._phase === 'active') {
      this.transitionTo('summarizing');
      this.emit('batch:allCompleted', this.id, this.getEntries());
    }
  }

  /**
   * 计算依赖链深度
   */
  private calculateDepth(taskId: string, visited: Set<string>): number {
    if (visited.has(taskId)) return 0;
    visited.add(taskId);

    const entry = this.entries.get(taskId);
    if (!entry || entry.dependsOn.length === 0) return 0;

    let maxDepth = 0;
    for (const depId of entry.dependsOn) {
      maxDepth = Math.max(maxDepth, this.calculateDepth(depId, visited) + 1);
    }
    return maxDepth;
  }
}
