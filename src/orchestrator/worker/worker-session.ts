/**
 * Worker Session 管理
 *
 * 用于保存和恢复 Worker 执行上下文
 * 实现提案 4.1: Session 恢复机制
 *
 * 核心功能：
 * - 保存对话历史和文件缓存
 * - 支持失败后从断点恢复
 * - 自动清理过期 Session
 */

import { WorkerSlot } from '../../types';
import { logger, LogCategory } from '../../logging';

// ============================================================================
// Session 类型定义
// ============================================================================

/**
 * 对话历史消息
 */
export interface ConversationMessage {
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: number;
}

/**
 * 文件缓存条目
 */
export interface FileCacheEntry {
  content: string;
  readAt: number;
  /** 文件哈希（用于检测变更） */
  hash?: string;
}

/**
 * Session 状态快照
 */
export interface SessionStateSnapshot {
  /** 当前 Todo 索引 */
  currentTodoIndex: number;
  /** 最后错误 */
  lastError?: string;
  /** 重试次数 */
  retryCount: number;
  /** 上次执行时间 */
  lastExecutionAt?: number;
}

/**
 * Worker Session
 */
export interface WorkerSession {
  /** Session ID */
  id: string;

  /** 关联的 Assignment ID */
  assignmentId: string;

  /** Worker 类型 */
  workerId: WorkerSlot;

  /** 对话历史（用于 LLM 上下文恢复） */
  conversationHistory: ConversationMessage[];

  /** 已读取的文件缓存 */
  readFiles: Map<string, FileCacheEntry>;

  /** 已完成的 Todo IDs */
  completedTodos: string[];

  /** 执行状态快照 */
  stateSnapshot: SessionStateSnapshot;

  /** 创建时间 */
  createdAt: number;

  /** 最后更新时间 */
  updatedAt: number;

  /** 是否已恢复（标记该 Session 是从失败中恢复的） */
  isResumed?: boolean;

  /** 恢复提示（用于恢复执行时的额外指令） */
  resumePrompt?: string;
}

/**
 * Session 创建选项
 */
export interface SessionCreateOptions {
  assignmentId: string;
  workerId: WorkerSlot;
  initialContext?: string;
}

/**
 * Session 更新选项
 */
export interface SessionUpdateOptions {
  /** 追加对话消息 */
  appendMessage?: ConversationMessage;
  /** 更新文件缓存 */
  updateFile?: { path: string; entry: FileCacheEntry };
  /** 标记 Todo 完成 */
  completeTodo?: string;
  /** 更新状态快照 */
  stateSnapshot?: Partial<SessionStateSnapshot>;
  /** 恢复提示 */
  resumePrompt?: string;
}

// ============================================================================
// WorkerSessionManager
// ============================================================================

/**
 * Session 存储管理器
 */
export class WorkerSessionManager {
  private sessions: Map<string, WorkerSession> = new Map();
  private readonly SESSION_TTL_MS: number;
  private readonly CLEANUP_INTERVAL_MS: number;
  private cleanupTimer: ReturnType<typeof setInterval> | null = null;

  constructor(options?: { sessionTtlMs?: number; cleanupIntervalMs?: number; autoCleanup?: boolean }) {
    this.SESSION_TTL_MS = options?.sessionTtlMs ?? 30 * 60 * 1000; // 默认 30 分钟
    this.CLEANUP_INTERVAL_MS = options?.cleanupIntervalMs ?? 5 * 60 * 1000; // 默认 5 分钟

    // 启动自动清理
    if (options?.autoCleanup !== false) {
      this.startAutoCleanup();
    }
  }

  /**
   * 生成唯一 Session ID
   */
  private generateSessionId(): string {
    const timestamp = Date.now().toString(36);
    const random = Math.random().toString(36).substring(2, 9);
    return `ses_${timestamp}_${random}`;
  }

  /**
   * 创建新 Session
   */
  create(options: SessionCreateOptions): WorkerSession {
    const id = this.generateSessionId();
    const now = Date.now();

    const session: WorkerSession = {
      id,
      assignmentId: options.assignmentId,
      workerId: options.workerId,
      conversationHistory: [],
      readFiles: new Map(),
      completedTodos: [],
      stateSnapshot: {
        currentTodoIndex: 0,
        retryCount: 0,
      },
      createdAt: now,
      updatedAt: now,
    };

    // 如果有初始上下文，添加为系统消息
    if (options.initialContext) {
      session.conversationHistory.push({
        role: 'system',
        content: options.initialContext,
        timestamp: now,
      });
    }

    this.sessions.set(id, session);

    logger.debug('Session.创建', {
      sessionId: id,
      assignmentId: options.assignmentId,
      workerId: options.workerId,
    }, LogCategory.ORCHESTRATOR);

    return session;
  }

  /**
   * 获取 Session
   */
  get(sessionId: string): WorkerSession | null {
    const session = this.sessions.get(sessionId);

    if (!session) {
      return null;
    }

    // 检查是否过期
    if (this.isExpired(session)) {
      this.delete(sessionId);
      return null;
    }

    return session;
  }

  /**
   * 根据 Assignment ID 获取 Session
   */
  getByAssignment(assignmentId: string): WorkerSession | null {
    for (const session of this.sessions.values()) {
      if (session.assignmentId === assignmentId && !this.isExpired(session)) {
        return session;
      }
    }
    return null;
  }

  /**
   * 更新 Session
   */
  update(sessionId: string, updates: SessionUpdateOptions): boolean {
    const session = this.get(sessionId);

    if (!session) {
      logger.warn('Session.更新失败.不存在', { sessionId }, LogCategory.ORCHESTRATOR);
      return false;
    }

    // 追加对话消息
    if (updates.appendMessage) {
      session.conversationHistory.push(updates.appendMessage);
    }

    // 更新文件缓存
    if (updates.updateFile) {
      session.readFiles.set(updates.updateFile.path, updates.updateFile.entry);
    }

    // 标记 Todo 完成
    if (updates.completeTodo && !session.completedTodos.includes(updates.completeTodo)) {
      session.completedTodos.push(updates.completeTodo);
    }

    // 更新状态快照
    if (updates.stateSnapshot) {
      session.stateSnapshot = {
        ...session.stateSnapshot,
        ...updates.stateSnapshot,
      };
    }

    // 更新恢复提示
    if (updates.resumePrompt !== undefined) {
      session.resumePrompt = updates.resumePrompt;
    }

    session.updatedAt = Date.now();

    logger.debug('Session.更新', {
      sessionId,
      hasMessage: !!updates.appendMessage,
      hasFile: !!updates.updateFile,
      completedTodo: updates.completeTodo,
    }, LogCategory.ORCHESTRATOR);

    return true;
  }

  /**
   * 标记 Session 为恢复状态
   */
  markAsResumed(sessionId: string, resumePrompt?: string): boolean {
    const session = this.get(sessionId);

    if (!session) {
      return false;
    }

    session.isResumed = true;
    session.resumePrompt = resumePrompt;
    session.stateSnapshot.retryCount += 1;
    session.updatedAt = Date.now();

    logger.info('Session.标记为恢复', {
      sessionId,
      retryCount: session.stateSnapshot.retryCount,
    }, LogCategory.ORCHESTRATOR);

    return true;
  }

  /**
   * 删除 Session
   */
  delete(sessionId: string): boolean {
    const existed = this.sessions.has(sessionId);
    this.sessions.delete(sessionId);

    if (existed) {
      logger.debug('Session.删除', { sessionId }, LogCategory.ORCHESTRATOR);
    }

    return existed;
  }

  /**
   * 检查 Session 是否过期
   */
  private isExpired(session: WorkerSession): boolean {
    return Date.now() - session.updatedAt > this.SESSION_TTL_MS;
  }

  /**
   * 清理过期 Session
   */
  cleanup(): number {
    let cleanedCount = 0;

    for (const [id, session] of this.sessions.entries()) {
      if (this.isExpired(session)) {
        this.sessions.delete(id);
        cleanedCount++;
      }
    }

    if (cleanedCount > 0) {
      logger.info('Session.清理过期', { count: cleanedCount }, LogCategory.ORCHESTRATOR);
    }

    return cleanedCount;
  }

  /**
   * 启动自动清理定时器
   */
  private startAutoCleanup(): void {
    this.cleanupTimer = setInterval(() => {
      this.cleanup();
    }, this.CLEANUP_INTERVAL_MS);
  }

  /**
   * 停止自动清理
   */
  stopAutoCleanup(): void {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
  }

  /**
   * 获取所有活跃 Session 数量
   */
  getActiveCount(): number {
    let count = 0;
    for (const session of this.sessions.values()) {
      if (!this.isExpired(session)) {
        count++;
      }
    }
    return count;
  }

  /**
   * 获取 Session 统计信息
   */
  getStats(): {
    total: number;
    active: number;
    resumed: number;
    byWorker: Record<string, number>;
  } {
    const stats = {
      total: this.sessions.size,
      active: 0,
      resumed: 0,
      byWorker: {} as Record<string, number>,
    };

    for (const session of this.sessions.values()) {
      if (!this.isExpired(session)) {
        stats.active++;
        if (session.isResumed) {
          stats.resumed++;
        }
        stats.byWorker[session.workerId] = (stats.byWorker[session.workerId] || 0) + 1;
      }
    }

    return stats;
  }

  /**
   * 清空所有 Session
   */
  clear(): void {
    this.sessions.clear();
    logger.info('Session.全部清空', {}, LogCategory.ORCHESTRATOR);
  }

  /**
   * 销毁管理器
   */
  dispose(): void {
    this.stopAutoCleanup();
    this.clear();
  }
}

// ============================================================================
// 导出单例（可选）
// ============================================================================

let globalSessionManager: WorkerSessionManager | null = null;

/**
 * 获取全局 Session 管理器
 */
export function getGlobalSessionManager(): WorkerSessionManager {
  if (!globalSessionManager) {
    globalSessionManager = new WorkerSessionManager();
  }
  return globalSessionManager;
}

/**
 * 重置全局 Session 管理器（用于测试）
 */
export function resetGlobalSessionManager(): void {
  if (globalSessionManager) {
    globalSessionManager.dispose();
    globalSessionManager = null;
  }
}
