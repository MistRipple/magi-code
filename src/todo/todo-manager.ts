/**
 * TodoManager - 统一的 Todo 管理器
 *
 * 职责：
 * - UnifiedTodo 完整生命周期管理
 * - 优先级调度
 * - 超时管理
 * - 契约依赖检查
 * - 范围检查与审批
 * - 事件驱动
 *
 * 设计原则：
 * - 单一数据源：替代 SubTask + WorkerTodo
 * - 高可用：持久化、超时、重试
 * - 事件驱动：通过 EventEmitter 通知外部
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../logging';
import { WorkerSlot } from '../types';
import {
  UnifiedTodo,
  TodoStatus,
  TodoType,
  TodoOutput,
  CreateTodoParams,
  UpdateTodoParams,
  TodoQuery,
  TodoStats,
  TodoPlanningContext,
  TodoPlanningResult,
  PlanReviewFeedback,
} from './types';
import { TodoRepository, FileTodoRepository } from './todo-repository';
import { PriorityQueue, PriorityItem } from '../task/priority-queue';
import { TimeoutChecker } from '../task/timeout-checker';

// ============================================================================
// 辅助类型
// ============================================================================

interface TodoPriorityItem extends PriorityItem {
  id: string;
  priority: number;
  todoId: string;
  missionId: string;
}

// ============================================================================
// 状态转换规则
// ============================================================================

const VALID_TRANSITIONS: Record<TodoStatus, TodoStatus[]> = {
  pending: ['blocked', 'ready', 'skipped'],
  blocked: ['ready', 'skipped'],
  ready: ['running', 'skipped'],
  running: ['completed', 'failed', 'blocked'],
  completed: [],
  failed: ['pending'], // 重试
  skipped: [],
};

// ============================================================================
// TodoManager
// ============================================================================

export class TodoManager extends EventEmitter {
  private repository: TodoRepository;
  private queue: PriorityQueue<TodoPriorityItem>;
  private timeoutChecker: TimeoutChecker;
  private cache: Map<string, UnifiedTodo> = new Map();

  /** 可用契约集合（已实现的契约） */
  private availableContracts: Set<string> = new Set();

  /** 缓存大小上限 */
  private static readonly MAX_CACHE_SIZE = 500;

  constructor(
    workspaceRoot: string,
    options?: {
      timeoutCheckInterval?: number;
    }
  ) {
    super();
    this.repository = new FileTodoRepository(workspaceRoot);
    this.queue = new PriorityQueue<TodoPriorityItem>();
    this.timeoutChecker = new TimeoutChecker(options?.timeoutCheckInterval);
  }

  /**
   * 初始化（从持久化层恢复状态）
   */
  async initialize(): Promise<void> {
    const todos = await this.repository.query({});
    for (const todo of todos) {
      this.cacheTodo(todo);

      // 恢复到优先级队列
      if (todo.status === 'ready') {
        this.enqueue(todo);
      }

      // 恢复超时监控
      if (todo.timeoutAt && todo.status === 'running') {
        this.timeoutChecker.add(todo.id, todo.timeoutAt, () => {
          this.handleTimeout(todo.id);
        });
      }
    }
    logger.info(
      'Todo.管理器.初始化',
      { count: todos.length },
      LogCategory.TASK
    );
  }

  /**
   * 销毁管理器
   */
  destroy(): void {
    this.timeoutChecker.destroy();
    this.cache.clear();
    this.queue.clear();
    this.availableContracts.clear();
    this.removeAllListeners();
  }

  // ============================================================================
  // 缓存管理
  // ============================================================================

  private cacheTodo(todo: UnifiedTodo): void {
    if (this.cache.has(todo.id)) {
      this.cache.delete(todo.id);
    }
    this.cache.set(todo.id, todo);

    while (this.cache.size > TodoManager.MAX_CACHE_SIZE) {
      const oldestId = this.cache.keys().next().value;
      if (oldestId) {
        this.cache.delete(oldestId);
      } else {
        break;
      }
    }
  }

  private getCached(todoId: string): UnifiedTodo | undefined {
    const todo = this.cache.get(todoId);
    if (todo) {
      this.cache.delete(todoId);
      this.cache.set(todoId, todo);
    }
    return todo;
  }

  // ============================================================================
  // 队列管理
  // ============================================================================

  private enqueue(todo: UnifiedTodo): void {
    this.queue.enqueue({
      id: todo.id,
      todoId: todo.id,
      missionId: todo.missionId,
      priority: todo.priority,
    });
  }

  // ============================================================================
  // CRUD 操作
  // ============================================================================

  /**
   * 创建 Todo
   */
  async create(params: CreateTodoParams): Promise<UnifiedTodo> {
    const now = Date.now();
    const todo: UnifiedTodo = {
      id: `todo_${now}_${Math.random().toString(36).substr(2, 9)}`,
      missionId: params.missionId,
      assignmentId: params.assignmentId,
      content: params.content,
      reasoning: params.reasoning,
      expectedOutput: params.expectedOutput,
      prompt: params.prompt,
      type: params.type,
      workerId: params.workerId,
      priority: params.priority ?? 3,
      dependsOn: params.dependsOn ?? [],
      requiredContracts: params.requiredContracts ?? [],
      producesContracts: params.producesContracts ?? [],
      outOfScope: false,
      status: 'pending',
      progress: 0,
      timeout: params.timeout,
      timeoutAt: params.timeout ? now + params.timeout : undefined,
      retryCount: 0,
      maxRetries: params.maxRetries ?? 3,
      targetFiles: params.targetFiles,
      createdAt: now,
    };

    await this.repository.save(todo);
    this.cacheTodo(todo);

    // 检查依赖，决定初始状态
    await this.checkAndUpdateStatus(todo);

    this.emit('todo:created', todo);
    return todo;
  }

  /**
   * 批量创建 Todos
   */
  async createBatch(paramsList: CreateTodoParams[]): Promise<UnifiedTodo[]> {
    const todos: UnifiedTodo[] = [];
    for (const params of paramsList) {
      const todo = await this.create(params);
      todos.push(todo);
    }
    return todos;
  }

  /**
   * 获取 Todo
   */
  async get(todoId: string): Promise<UnifiedTodo | null> {
    const cached = this.getCached(todoId);
    if (cached) return cached;

    const todo = await this.repository.get(todoId);
    if (todo) {
      this.cacheTodo(todo);
    }
    return todo;
  }

  /**
   * 更新 Todo
   */
  async update(todoId: string, updates: UpdateTodoParams): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    Object.assign(todo, updates);
    await this.repository.save(todo);
  }

  /**
   * 删除 Todo
   */
  async delete(todoId: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) return;

    this.timeoutChecker.remove(todoId);
    this.queue.remove(todoId);
    this.cache.delete(todoId);
    await this.repository.delete(todoId);
  }

  // ============================================================================
  // 查询接口
  // ============================================================================

  /**
   * 按 Mission 获取所有 Todos
   */
  async getByMission(missionId: string): Promise<UnifiedTodo[]> {
    return await this.repository.getByMission(missionId);
  }

  /**
   * 按 Assignment 获取所有 Todos
   */
  async getByAssignment(assignmentId: string): Promise<UnifiedTodo[]> {
    return await this.repository.getByAssignment(assignmentId);
  }

  /**
   * 按状态获取 Todos
   */
  async getByStatus(status: TodoStatus | TodoStatus[]): Promise<UnifiedTodo[]> {
    return await this.repository.getByStatus(status);
  }

  /**
   * 复杂查询
   */
  async query(query: TodoQuery): Promise<UnifiedTodo[]> {
    return await this.repository.query(query);
  }

  // ============================================================================
  // 状态管理
  // ============================================================================

  /**
   * 检查并更新 Todo 状态
   * 根据依赖和契约状态，自动转换 pending → blocked 或 ready
   */
  private async checkAndUpdateStatus(todo: UnifiedTodo): Promise<void> {
    if (todo.status !== 'pending' && todo.status !== 'blocked') {
      return;
    }

    // 检查 Todo 依赖
    const dependencyMet = await this.checkDependencies(todo);

    // 检查契约依赖
    const contractsMet = this.checkContracts(todo);

    // 检查范围审批
    const approvalMet = !todo.outOfScope || todo.approvalStatus === 'approved';

    if (!dependencyMet) {
      await this.block(todo.id, '等待前置 Todo 完成');
    } else if (!contractsMet) {
      await this.block(todo.id, '等待契约: ' + todo.requiredContracts.join(', '));
    } else if (!approvalMet) {
      await this.block(todo.id, '等待超范围审批');
    } else {
      await this.setReady(todo.id);
    }
  }

  /**
   * 检查 Todo 依赖是否满足
   */
  private async checkDependencies(todo: UnifiedTodo): Promise<boolean> {
    if (todo.dependsOn.length === 0) return true;

    for (const depId of todo.dependsOn) {
      const dep = await this.get(depId);
      if (!dep || dep.status !== 'completed') {
        return false;
      }
    }
    return true;
  }

  /**
   * 检查契约依赖是否满足
   */
  private checkContracts(todo: UnifiedTodo): boolean {
    if (todo.requiredContracts.length === 0) return true;

    for (const contract of todo.requiredContracts) {
      if (!this.availableContracts.has(contract)) {
        return false;
      }
    }
    return true;
  }

  /**
   * 注册可用契约
   */
  registerContract(contractId: string): void {
    this.availableContracts.add(contractId);
    // 触发等待此契约的 Todos 重新检查
    this.recheckBlockedTodos();
  }

  /**
   * 重新检查所有阻塞的 Todos
   */
  private async recheckBlockedTodos(): Promise<void> {
    const blocked = await this.getByStatus('blocked');
    for (const todo of blocked) {
      await this.checkAndUpdateStatus(todo);
    }
  }

  /**
   * 设置为阻塞状态
   */
  private async block(todoId: string, reason: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) return;
    if (todo.status === 'blocked' && todo.blockedReason === reason) return;

    if (!VALID_TRANSITIONS[todo.status].includes('blocked')) {
      return;
    }

    todo.status = 'blocked';
    todo.blockedReason = reason;
    await this.repository.save(todo);
    this.emit('todo:blocked', todo, reason);
  }

  /**
   * 设置为就绪状态
   */
  private async setReady(todoId: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) return;
    if (todo.status === 'ready') return;

    if (!VALID_TRANSITIONS[todo.status].includes('ready')) {
      return;
    }

    todo.status = 'ready';
    todo.blockedReason = undefined;
    await this.repository.save(todo);

    this.enqueue(todo);
    this.emit('todo:ready', todo);
    this.emit('todo:unblocked', todo);
  }

  /**
   * 启动 Todo
   */
  async start(todoId: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    if (todo.status !== 'ready') {
      throw new Error(`Cannot start todo in status: ${todo.status}`);
    }

    todo.status = 'running';
    todo.startedAt = Date.now();
    this.queue.remove(todoId);

    // 设置超时监控
    if (todo.timeoutAt) {
      this.timeoutChecker.add(todoId, todo.timeoutAt, () => {
        this.handleTimeout(todoId);
      });
    }

    await this.repository.save(todo);
    this.emit('todo:started', todo);
  }

  /**
   * 更新进度
   */
  async updateProgress(todoId: string, progress: number): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    todo.progress = Math.max(0, Math.min(100, progress));
    await this.repository.save(todo);
    this.emit('todo:progress', todo, progress);
  }

  /**
   * 完成 Todo
   */
  async complete(todoId: string, output?: TodoOutput): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    if (todo.status !== 'running') {
      throw new Error(`Cannot complete todo in status: ${todo.status}`);
    }

    todo.status = 'completed';
    todo.progress = 100;
    todo.completedAt = Date.now();
    todo.output = output;
    if (output?.modifiedFiles) {
      todo.modifiedFiles = output.modifiedFiles;
    }

    this.timeoutChecker.remove(todoId);

    // 注册此 Todo 产生的契约
    for (const contract of todo.producesContracts) {
      this.registerContract(contract);
    }

    await this.repository.save(todo);
    this.emit('todo:completed', todo);

    // 触发依赖此 Todo 的其他 Todos 检查
    await this.triggerDependentTodos(todoId);
  }

  /**
   * 失败 Todo
   */
  async fail(todoId: string, error: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    todo.status = 'failed';
    todo.completedAt = Date.now();
    todo.error = error;

    this.timeoutChecker.remove(todoId);
    await this.repository.save(todo);
    this.emit('todo:failed', todo, error);
  }

  /**
   * 跳过 Todo
   */
  async skip(todoId: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    if (!VALID_TRANSITIONS[todo.status].includes('skipped')) {
      throw new Error(`Cannot skip todo in status: ${todo.status}`);
    }

    todo.status = 'skipped';
    todo.completedAt = Date.now();

    this.timeoutChecker.remove(todoId);
    this.queue.remove(todoId);
    await this.repository.save(todo);
    this.emit('todo:skipped', todo);

    // 触发依赖此 Todo 的其他 Todos 检查
    await this.triggerDependentTodos(todoId);
  }

  /**
   * 重试 Todo
   */
  async retry(todoId: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    if (todo.status !== 'failed') {
      throw new Error(`Cannot retry todo in status: ${todo.status}`);
    }

    if (todo.retryCount >= todo.maxRetries) {
      throw new Error(`Todo has reached max retries: ${todo.maxRetries}`);
    }

    todo.status = 'pending';
    todo.retryCount++;
    todo.error = undefined;
    todo.progress = 0;
    todo.completedAt = undefined;

    await this.repository.save(todo);
    this.emit('todo:retrying', todo);

    // 重新检查状态
    await this.checkAndUpdateStatus(todo);
  }

  /**
   * 触发依赖此 Todo 的其他 Todos 检查
   */
  private async triggerDependentTodos(todoId: string): Promise<void> {
    const allTodos = await this.repository.query({});
    for (const todo of allTodos) {
      if (todo.dependsOn.includes(todoId)) {
        await this.checkAndUpdateStatus(todo);
      }
    }
  }

  // ============================================================================
  // 公共检查方法（用于外部调用和测试）
  // ============================================================================

  /**
   * 准备 Todo 执行（检查依赖并更新状态）
   * 如果依赖满足，状态变为 ready；否则变为 blocked
   */
  async prepareForExecution(todoId: string): Promise<boolean> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    await this.checkAndUpdateStatus(todo);

    const updated = await this.get(todoId);
    return updated?.status === 'ready';
  }

  /**
   * 检查 Todo 是否可以执行
   */
  async canExecute(todoId: string): Promise<boolean> {
    const todo = await this.get(todoId);
    if (!todo) return false;

    // 检查 Todo 依赖
    for (const depId of todo.dependsOn) {
      const dep = await this.get(depId);
      if (!dep || dep.status !== 'completed') {
        return false;
      }
    }

    // 检查契约
    for (const contract of todo.requiredContracts) {
      if (!this.availableContracts.has(contract)) {
        return false;
      }
    }

    // 检查审批
    if (todo.outOfScope && todo.approvalStatus !== 'approved') {
      return false;
    }

    return true;
  }

  /**
   * 处理超时
   */
  private async handleTimeout(todoId: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo || todo.status !== 'running') return;

    await this.fail(todoId, 'Todo timeout');
    this.emit('todo:timeout', todo);
  }

  // ============================================================================
  // 队列操作
  // ============================================================================

  /**
   * 获取下一个就绪的 Todo（不移除）
   */
  peek(): UnifiedTodo | null {
    const item = this.queue.peek();
    if (!item) return null;
    return this.cache.get(item.todoId) || null;
  }

  /**
   * 出队下一个就绪的 Todo
   */
  dequeue(): UnifiedTodo | null {
    const item = this.queue.dequeue();
    if (!item) return null;
    return this.cache.get(item.todoId) || null;
  }

  /**
   * 批量出队多个 Todos
   */
  dequeueBatch(count: number): UnifiedTodo[] {
    const results: UnifiedTodo[] = [];
    for (let i = 0; i < count; i++) {
      const todo = this.dequeue();
      if (!todo) break;
      results.push(todo);
    }
    return results;
  }

  // ============================================================================
  // 范围检查与审批
  // ============================================================================

  /**
   * 检查范围
   */
  checkScope(
    todo: UnifiedTodo,
    scope: { includes: string[]; excludes: string[] }
  ): { outOfScope: boolean; reason?: string } {
    const content = todo.content.toLowerCase();

    for (const exclude of scope.excludes) {
      if (content.includes(exclude.toLowerCase())) {
        return { outOfScope: true, reason: `涉及排除项: "${exclude}"` };
      }
    }

    return { outOfScope: false };
  }

  /**
   * 请求超范围审批
   */
  async requestApproval(todoId: string, note?: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    todo.outOfScope = true;
    todo.approvalStatus = 'pending';
    todo.approvalNote = note;

    await this.repository.save(todo);
    await this.checkAndUpdateStatus(todo);
    this.emit('todo:approval-requested', todo);
  }

  /**
   * 批准超范围任务
   */
  async approve(todoId: string, note?: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    todo.approvalStatus = 'approved';
    if (note) todo.approvalNote = note;

    await this.repository.save(todo);
    await this.checkAndUpdateStatus(todo);
    this.emit('todo:approved', todo);
  }

  /**
   * 拒绝超范围任务
   */
  async reject(todoId: string, reason: string): Promise<void> {
    const todo = await this.get(todoId);
    if (!todo) throw new Error(`Todo not found: ${todoId}`);

    todo.approvalStatus = 'rejected';
    todo.approvalNote = reason;

    await this.repository.save(todo);
    await this.skip(todoId);
    this.emit('todo:rejected', todo);
  }

  // ============================================================================
  // 规划功能
  // ============================================================================

  /**
   * 为 Assignment 生成 Todo 规划
   */
  async planTodos(context: TodoPlanningContext): Promise<TodoPlanningResult> {
    const todos: UnifiedTodo[] = [];
    const outOfScopeTodos: UnifiedTodo[] = [];
    const warnings: string[] = [];
    const responsibility = context.responsibility.toLowerCase();

    // 1. 探索 Todo
    if (this.needsDiscovery(responsibility)) {
      const todo = await this.create({
        missionId: context.missionId,
        assignmentId: context.assignmentId,
        content: '探索和了解相关代码结构',
        reasoning: '在实现前需要了解现有代码',
        type: 'discovery',
        workerId: context.workerId,
        priority: 1,
      });
      todos.push(todo);
    }

    // 2. 设计 Todo
    if (this.needsDesign(responsibility)) {
      const todo = await this.create({
        missionId: context.missionId,
        assignmentId: context.assignmentId,
        content: '设计实现方案',
        reasoning: '需要先规划实现路径',
        type: 'design',
        workerId: context.workerId,
        priority: 2,
        dependsOn: todos.length > 0 ? [todos[todos.length - 1].id] : [],
      });
      todos.push(todo);
    }

    // 3. 实现 Todo
    const targetPaths = context.scope.targetPaths?.length
      ? context.scope.requiresModification
        ? `目标文件: ${context.scope.targetPaths.join(', ')}。必须使用工具直接编辑并保存。`
        : `目标文件: ${context.scope.targetPaths.join(', ')}。只需读取/分析，不要修改文件。`
      : '';

    const implTodo = await this.create({
      missionId: context.missionId,
      assignmentId: context.assignmentId,
      content: `执行职责任务：${context.responsibility}${targetPaths ? `\n${targetPaths}` : ''}`,
      reasoning: '完成职责的主要工作',
      expectedOutput: context.responsibility,
      type: 'implementation',
      workerId: context.workerId,
      priority: 3,
      dependsOn: todos.length > 0 ? [todos[todos.length - 1].id] : [],
      targetFiles: context.scope.targetPaths,
    });
    todos.push(implTodo);

    // 4. 验证 Todo
    const verifyTodo = await this.create({
      missionId: context.missionId,
      assignmentId: context.assignmentId,
      content: `验证任务结果是否满足目标：${context.responsibility}`,
      reasoning: '确保实现正确',
      type: 'verification',
      workerId: context.workerId,
      priority: 4,
      dependsOn: [implTodo.id],
    });
    todos.push(verifyTodo);

    // 检查范围
    for (const todo of todos) {
      const scopeCheck = this.checkScope(todo, context.scope);
      if (scopeCheck.outOfScope) {
        todo.outOfScope = true;
        todo.approvalStatus = 'pending';
        todo.approvalNote = scopeCheck.reason;
        await this.repository.save(todo);
        outOfScopeTodos.push(todo);
        warnings.push(`Todo "${todo.content}" 超出职责范围: ${scopeCheck.reason}`);
      }
    }

    return { todos, outOfScopeTodos, warnings };
  }

  private needsDiscovery(responsibility: string): boolean {
    const keywords = ['理解', '了解', '分析', '探索', 'understand', 'analyze', 'explore'];
    return keywords.some((k) => responsibility.includes(k));
  }

  private needsDesign(responsibility: string): boolean {
    const keywords = ['新增', '创建', '设计', '实现', 'new', 'create', 'design', 'implement'];
    return keywords.some((k) => responsibility.includes(k));
  }

  /**
   * 应用规划修订
   */
  async revisePlan(
    missionId: string,
    feedback: PlanReviewFeedback
  ): Promise<{
    todosAdded: number;
    todosRemoved: number;
    todosModified: number;
  }> {
    let todosAdded = 0;
    let todosRemoved = 0;
    let todosModified = 0;

    // 1. 移除指定的 Todos
    if (feedback.todosToRemove?.length) {
      for (const todoId of feedback.todosToRemove) {
        await this.delete(todoId);
        todosRemoved++;
      }
    }

    // 2. 修改指定的 Todos
    if (feedback.todosToModify?.length) {
      for (const mod of feedback.todosToModify) {
        await this.update(mod.todoId, mod.updates);
        todosModified++;
      }
    }

    // 3. 添加新的 Todos
    if (feedback.todosToAdd?.length) {
      for (const params of feedback.todosToAdd) {
        await this.create({ ...params, missionId });
        todosAdded++;
      }
    }

    return { todosAdded, todosRemoved, todosModified };
  }

  // ============================================================================
  // 统计与维护
  // ============================================================================

  /**
   * 获取统计信息
   */
  async getStats(): Promise<TodoStats> {
    return await this.repository.getStats();
  }

  /**
   * 清理旧数据
   */
  async cleanup(olderThan: number): Promise<number> {
    return await this.repository.cleanup(olderThan);
  }

  /**
   * 检查 Mission 是否完成
   */
  async checkMissionCompletion(missionId: string): Promise<{
    allDone: boolean;
    anyFailed: boolean;
    stats: { completed: number; failed: number; pending: number; total: number };
  }> {
    const todos = await this.getByMission(missionId);
    const stats = {
      completed: 0,
      failed: 0,
      pending: 0,
      total: todos.length,
    };

    for (const todo of todos) {
      if (todo.status === 'completed' || todo.status === 'skipped') {
        stats.completed++;
      } else if (todo.status === 'failed') {
        stats.failed++;
      } else {
        stats.pending++;
      }
    }

    return {
      allDone: stats.pending === 0 && stats.failed === 0,
      anyFailed: stats.failed > 0,
      stats,
    };
  }
}
