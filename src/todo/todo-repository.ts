/**
 * TodoRepository - Todo 持久化层
 *
 * 职责：
 * - UnifiedTodo 数据的持久化存储
 * - 查询接口（按 Mission、Assignment、状态等）
 * - 事务支持
 * - 数据恢复
 */

import { logger, LogCategory } from '../logging';
import {
  UnifiedTodo,
  TodoStatus,
  TodoType,
  TodoQuery,
  TodoStats,
} from './types';
import { WorkerSlot } from '../types';
import * as fs from 'fs';
import * as path from 'path';

// ============================================================================
// 事务接口
// ============================================================================

export interface TodoTransaction {
  id: string;
  startedAt: number;
  snapshot: Map<string, UnifiedTodo>;
}

// ============================================================================
// TodoRepository 接口
// ============================================================================

export interface TodoRepository {
  // ===== CRUD =====
  save(todo: UnifiedTodo): Promise<void>;
  saveBatch(todos: UnifiedTodo[]): Promise<void>;
  get(todoId: string): Promise<UnifiedTodo | null>;
  delete(todoId: string): Promise<void>;
  deleteBatch(todoIds: string[]): Promise<void>;

  // ===== 查询 =====
  getByMission(missionId: string): Promise<UnifiedTodo[]>;
  getByAssignment(assignmentId: string): Promise<UnifiedTodo[]>;
  getByStatus(status: TodoStatus | TodoStatus[]): Promise<UnifiedTodo[]>;
  getByWorker(workerId: WorkerSlot): Promise<UnifiedTodo[]>;
  query(query: TodoQuery): Promise<UnifiedTodo[]>;

  // ===== 事务 =====
  beginTransaction(): Promise<TodoTransaction>;
  commitTransaction(tx: TodoTransaction): Promise<void>;
  rollbackTransaction(tx: TodoTransaction): Promise<void>;

  // ===== 维护 =====
  cleanup(olderThan: number): Promise<number>;
  getStats(): Promise<TodoStats>;
}

// ============================================================================
// 文件存储实现
// ============================================================================

/**
 * 基于文件系统的 TodoRepository 实现
 */
export class FileTodoRepository implements TodoRepository {
  private workspaceRoot: string;
  private storageDir: string;
  private todosFile: string;
  private cache: Map<string, UnifiedTodo> = new Map();
  private dirty: boolean = false;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.storageDir = path.join(workspaceRoot, '.multicli', 'todos');
    this.todosFile = path.join(this.storageDir, 'todos.json');
    this.ensureStorageDir();
    this.loadCache();
  }

  private ensureStorageDir(): void {
    if (!fs.existsSync(this.storageDir)) {
      fs.mkdirSync(this.storageDir, { recursive: true });
    }
  }

  private loadCache(): void {
    if (fs.existsSync(this.todosFile)) {
      try {
        const data = fs.readFileSync(this.todosFile, 'utf-8');
        const todos: UnifiedTodo[] = JSON.parse(data);
        for (const todo of todos) {
          this.cache.set(todo.id, todo);
        }
        logger.debug(
          'Todo.仓库.缓存_加载',
          { count: todos.length },
          LogCategory.TASK
        );
      } catch (error) {
        logger.error('Todo.仓库.缓存_加载_失败', error, LogCategory.TASK);
      }
    }
  }

  private async persist(): Promise<void> {
    if (!this.dirty) return;

    const todos = Array.from(this.cache.values());
    const data = JSON.stringify(todos, null, 2);
    fs.writeFileSync(this.todosFile, data, 'utf-8');
    this.dirty = false;
  }

  // ===== CRUD =====

  async save(todo: UnifiedTodo): Promise<void> {
    this.cache.set(todo.id, todo);
    this.dirty = true;
    await this.persist();
  }

  async saveBatch(todos: UnifiedTodo[]): Promise<void> {
    for (const todo of todos) {
      this.cache.set(todo.id, todo);
    }
    this.dirty = true;
    await this.persist();
  }

  async get(todoId: string): Promise<UnifiedTodo | null> {
    return this.cache.get(todoId) || null;
  }

  async delete(todoId: string): Promise<void> {
    this.cache.delete(todoId);
    this.dirty = true;
    await this.persist();
  }

  async deleteBatch(todoIds: string[]): Promise<void> {
    for (const id of todoIds) {
      this.cache.delete(id);
    }
    this.dirty = true;
    await this.persist();
  }

  // ===== 查询 =====

  async getByMission(missionId: string): Promise<UnifiedTodo[]> {
    return Array.from(this.cache.values()).filter(
      (t) => t.missionId === missionId
    );
  }

  async getByAssignment(assignmentId: string): Promise<UnifiedTodo[]> {
    return Array.from(this.cache.values()).filter(
      (t) => t.assignmentId === assignmentId
    );
  }

  async getByStatus(status: TodoStatus | TodoStatus[]): Promise<UnifiedTodo[]> {
    const statuses = Array.isArray(status) ? status : [status];
    return Array.from(this.cache.values()).filter((t) =>
      statuses.includes(t.status)
    );
  }

  async getByWorker(workerId: WorkerSlot): Promise<UnifiedTodo[]> {
    return Array.from(this.cache.values()).filter(
      (t) => t.workerId === workerId
    );
  }

  async query(query: TodoQuery): Promise<UnifiedTodo[]> {
    let todos = Array.from(this.cache.values());

    if (query.missionId) {
      todos = todos.filter((t) => t.missionId === query.missionId);
    }

    if (query.assignmentId) {
      todos = todos.filter((t) => t.assignmentId === query.assignmentId);
    }

    if (query.workerId) {
      todos = todos.filter((t) => t.workerId === query.workerId);
    }

    if (query.status) {
      const statuses = Array.isArray(query.status)
        ? query.status
        : [query.status];
      todos = todos.filter((t) => statuses.includes(t.status));
    }

    if (query.type) {
      const types = Array.isArray(query.type) ? query.type : [query.type];
      todos = todos.filter((t) => types.includes(t.type));
    }

    if (query.outOfScope !== undefined) {
      todos = todos.filter((t) => t.outOfScope === query.outOfScope);
    }

    return todos;
  }

  // ===== 事务 =====

  async beginTransaction(): Promise<TodoTransaction> {
    return {
      id: `tx-${Date.now()}`,
      startedAt: Date.now(),
      snapshot: new Map(this.cache),
    };
  }

  async commitTransaction(tx: TodoTransaction): Promise<void> {
    await this.persist();
  }

  async rollbackTransaction(tx: TodoTransaction): Promise<void> {
    this.cache = new Map(tx.snapshot);
    this.dirty = false;
  }

  // ===== 维护 =====

  async cleanup(olderThan: number): Promise<number> {
    const todos = Array.from(this.cache.values());
    const toDelete = todos.filter(
      (t) =>
        (t.status === 'completed' ||
          t.status === 'failed' ||
          t.status === 'skipped') &&
        t.createdAt < olderThan
    );

    for (const todo of toDelete) {
      this.cache.delete(todo.id);
    }

    if (toDelete.length > 0) {
      this.dirty = true;
      await this.persist();
    }

    return toDelete.length;
  }

  async getStats(): Promise<TodoStats> {
    const todos = Array.from(this.cache.values());

    const byStatus: Record<TodoStatus, number> = {
      pending: 0,
      blocked: 0,
      ready: 0,
      running: 0,
      completed: 0,
      failed: 0,
      skipped: 0,
    };

    const byType: Record<TodoType, number> = {
      discovery: 0,
      design: 0,
      implementation: 0,
      verification: 0,
      integration: 0,
      fix: 0,
      refactor: 0,
    };

    const byWorker: Record<string, number> = {};

    let totalDuration = 0;
    let completedCount = 0;

    for (const todo of todos) {
      byStatus[todo.status]++;
      byType[todo.type]++;
      byWorker[todo.workerId] = (byWorker[todo.workerId] || 0) + 1;

      if (
        todo.status === 'completed' &&
        todo.startedAt &&
        todo.completedAt
      ) {
        totalDuration += todo.completedAt - todo.startedAt;
        completedCount++;
      }
    }

    const completedAndSkipped =
      byStatus.completed + byStatus.skipped;
    const completionRate =
      todos.length > 0 ? completedAndSkipped / todos.length : 0;
    const averageDuration =
      completedCount > 0 ? totalDuration / completedCount : 0;

    return {
      total: todos.length,
      byStatus,
      byType,
      byWorker,
      completionRate,
      averageDuration,
    };
  }
}
