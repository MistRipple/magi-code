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
import { atomicWriteFileSync } from '../utils/atomic-write';

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
 * 存储结构：.magi/sessions/{sessionId}/todos.json
 */
export class FileTodoRepository implements TodoRepository {
  private workspaceRoot: string;
  private sessionsDir: string;
  private cache: Map<string, UnifiedTodo> = new Map();
  private dirtySessions: Set<string> = new Set();

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.sessionsDir = path.join(workspaceRoot, '.magi', 'sessions');
    this.ensureSessionsDir();
    this.loadCache();
  }

  private ensureSessionsDir(): void {
    if (!fs.existsSync(this.sessionsDir)) {
      fs.mkdirSync(this.sessionsDir, { recursive: true });
    }
  }

  private getSessionTodosFile(sessionId: string): string {
    return path.join(this.sessionsDir, sessionId, 'todos.json');
  }

  private ensureSessionDir(sessionId: string): void {
    const sessionDir = path.join(this.sessionsDir, sessionId);
    if (!fs.existsSync(sessionDir)) {
      fs.mkdirSync(sessionDir, { recursive: true });
    }
  }

  private normalizeSessionId(sessionId: string): string {
    const normalized = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (!normalized) {
      throw new Error('Todo.sessionId 不能为空');
    }
    return normalized;
  }

  private markSessionDirty(sessionId: string | undefined): void {
    if (!sessionId || typeof sessionId !== 'string') {
      return;
    }
    const normalized = sessionId.trim();
    if (!normalized) {
      return;
    }
    this.dirtySessions.add(normalized);
  }

  private loadCache(): void {
    if (!fs.existsSync(this.sessionsDir)) {
      return;
    }

    try {
      const entries = fs.readdirSync(this.sessionsDir, { withFileTypes: true });
      let loadedCount = 0;

      for (const entry of entries) {
        if (!entry.isDirectory()) {
          continue;
        }

        const sessionId = entry.name;
        const todosFile = this.getSessionTodosFile(sessionId);
        if (!fs.existsSync(todosFile)) {
          continue;
        }

        try {
          const data = fs.readFileSync(todosFile, 'utf-8');
          const todos = JSON.parse(data);
          if (!Array.isArray(todos)) {
            continue;
          }
          for (const rawTodo of todos) {
            if (!rawTodo || typeof rawTodo !== 'object') {
              continue;
            }
            const todo = {
              ...(rawTodo as Omit<UnifiedTodo, 'sessionId'>),
              sessionId: typeof (rawTodo as { sessionId?: unknown }).sessionId === 'string'
                ? (rawTodo as { sessionId: string }).sessionId.trim() || sessionId
                : sessionId,
            } as UnifiedTodo;
            this.cache.set(todo.id, todo);
            loadedCount++;
          }
        } catch (error) {
          logger.error('Todo.仓库.缓存_加载_失败', {
            sessionId,
            error,
          }, LogCategory.TASK);
        }
      }

      logger.debug(
        'Todo.仓库.缓存_加载',
        { count: loadedCount },
        LogCategory.TASK
      );
    } catch (error) {
      logger.error('Todo.仓库.缓存_加载_失败', error, LogCategory.TASK);
    }
  }

  private async persist(): Promise<void> {
    if (this.dirtySessions.size === 0) {
      return;
    }

    const sessionsToPersist = Array.from(this.dirtySessions);
    this.dirtySessions.clear();

    for (const sessionId of sessionsToPersist) {
      this.ensureSessionDir(sessionId);
      const todos = Array.from(this.cache.values()).filter(
        (todo) => todo.sessionId === sessionId
      );
      const data = JSON.stringify(todos, null, 2);
      atomicWriteFileSync(this.getSessionTodosFile(sessionId), data);
    }
  }

  // ===== CRUD =====

  async save(todo: UnifiedTodo): Promise<void> {
    const sessionId = this.normalizeSessionId(todo.sessionId);
    const normalizedTodo: UnifiedTodo = {
      ...todo,
      sessionId,
    };
    this.cache.set(normalizedTodo.id, normalizedTodo);
    this.markSessionDirty(sessionId);
    await this.persist();
  }

  async saveBatch(todos: UnifiedTodo[]): Promise<void> {
    for (const todo of todos) {
      const sessionId = this.normalizeSessionId(todo.sessionId);
      const normalizedTodo: UnifiedTodo = {
        ...todo,
        sessionId,
      };
      this.cache.set(normalizedTodo.id, normalizedTodo);
      this.markSessionDirty(sessionId);
    }
    await this.persist();
  }

  async get(todoId: string): Promise<UnifiedTodo | null> {
    return this.cache.get(todoId) || null;
  }

  async delete(todoId: string): Promise<void> {
    const existing = this.cache.get(todoId);
    this.cache.delete(todoId);
    this.markSessionDirty(existing?.sessionId);
    await this.persist();
  }

  async deleteBatch(todoIds: string[]): Promise<void> {
    for (const id of todoIds) {
      const existing = this.cache.get(id);
      this.cache.delete(id);
      this.markSessionDirty(existing?.sessionId);
    }
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

    const sessionFilter = typeof query.sessionId === 'string' ? query.sessionId.trim() : '';
    if (sessionFilter) {
      todos = todos.filter((t) => t.sessionId === sessionFilter);
    }

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
    // 深拷贝 cache，避免事务期间共享引用导致快照被污染
    const snapshot = new Map<string, UnifiedTodo>();
    for (const [id, todo] of this.cache) {
      snapshot.set(id, { ...todo });
    }
    return {
      id: `tx-${Date.now()}`,
      startedAt: Date.now(),
      snapshot,
    };
  }

  async commitTransaction(_tx: TodoTransaction): Promise<void> {
    await this.persist();
  }

  async rollbackTransaction(tx: TodoTransaction): Promise<void> {
    this.cache = new Map(tx.snapshot);
    // 将回滚后的状态持久化到磁盘，保证内存与磁盘一致
    const allSessionIds = new Set<string>();
    for (const todo of this.cache.values()) {
      if (todo.sessionId) {
        allSessionIds.add(todo.sessionId);
      }
    }
    for (const sessionId of allSessionIds) {
      this.dirtySessions.add(sessionId);
    }
    await this.persist();
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
      this.markSessionDirty(todo.sessionId);
    }

    if (toDelete.length > 0) await this.persist();

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
