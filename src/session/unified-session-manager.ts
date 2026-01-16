/**
 * 统一会话管理器
 * 将所有会话相关数据按会话ID组织存储
 * 
 * 目录结构：
 * .multicli/sessions/{sessionId}/
 * ├── session.json          # 会话主数据
 * ├── plans/                # 计划文件
 * ├── tasks.json            # 子任务状态
 * ├── snapshots/            # 快照文件
 * └── execution-state.json  # 执行状态
 */

import * as fs from 'fs';
import * as path from 'path';
import { CLIType, Task, FileSnapshot } from '../types';
import { globalEventBus } from '../events';

/** 会话消息 */
export interface SessionMessage {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  cli?: CLIType;
  source?: 'orchestrator' | 'worker' | 'system';
  timestamp: number;
  attachments?: { name: string; path: string; mimeType?: string }[];
}

/** 文件快照元数据 */
export interface FileSnapshotMeta {
  id: string;
  filePath: string;
  lastModifiedBy: CLIType;
  lastModifiedAt: number;
  subTaskId: string;
}

/** 任务状态 */
export type SessionStatus = 'active' | 'completed';

/** 统一会话数据结构 */
export interface UnifiedSession {
  id: string;
  name?: string;
  status: SessionStatus;
  createdAt: number;
  updatedAt: number;
  /** 聊天消息 */
  messages: SessionMessage[];
  /** 任务列表 */
  tasks: Task[];
  /** 快照元数据 */
  snapshots: FileSnapshotMeta[];
  /** CLI 会话 ID 映射 */
  cliSessionIds?: Record<string, string>;
  /** CLI 输出缓存 */
  cliOutputs?: Record<string, any[]>;
}

/** 会话元数据（用于列表显示） */
export interface SessionMeta {
  id: string;
  name?: string;
  messageCount: number;
  createdAt: number;
  updatedAt: number;
  preview: string;
}

/** 生成唯一 ID */
function generateId(): string {
  return `session-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/** 生成消息 ID */
function generateMessageId(): string {
  return `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`;
}

/**
 * 统一会话管理器
 */
export class UnifiedSessionManager {
  private sessions: Map<string, UnifiedSession> = new Map();
  private currentSessionId: string | null = null;
  private workspaceRoot: string;
  private baseDir: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.baseDir = path.join(workspaceRoot, '.multicli', 'sessions');
    this.ensureBaseDir();
    this.loadAllSessions();
  }

  /** 确保基础目录存在 */
  private ensureBaseDir(): void {
    if (!fs.existsSync(this.baseDir)) {
      fs.mkdirSync(this.baseDir, { recursive: true });
    }
  }

  /** 获取会话目录路径 */
  getSessionDir(sessionId: string): string {
    return path.join(this.baseDir, sessionId);
  }

  /** 确保会话目录结构存在 */
  private ensureSessionDir(sessionId: string): void {
    const sessionDir = this.getSessionDir(sessionId);
    const dirs = [
      sessionDir,
      path.join(sessionDir, 'plans'),
      path.join(sessionDir, 'snapshots'),
    ];
    for (const dir of dirs) {
      if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
      }
    }
  }

  /** 获取会话文件路径 */
  private getSessionFilePath(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'session.json');
  }

  /** 创建新会话 */
  createSession(name?: string, sessionId?: string): UnifiedSession {
    if (sessionId && this.sessions.has(sessionId)) {
      this.currentSessionId = sessionId;
      return this.sessions.get(sessionId)!;
    }

    const now = Date.now();
    const id = sessionId ?? generateId();

    const session: UnifiedSession = {
      id,
      name: name || undefined,
      status: 'active',
      createdAt: now,
      updatedAt: now,
      messages: [],
      tasks: [],
      snapshots: [],
    };

    this.ensureSessionDir(id);
    this.sessions.set(id, session);
    this.currentSessionId = id;
    this.saveSession(session);

    globalEventBus.emitEvent('session:created', { sessionId: id });
    return session;
  }

  /** 获取当前会话 */
  getCurrentSession(): UnifiedSession | null {
    if (!this.currentSessionId) return null;
    return this.sessions.get(this.currentSessionId) ?? null;
  }

  /** 获取或创建当前会话 */
  getOrCreateCurrentSession(): UnifiedSession {
    const current = this.getCurrentSession();
    if (current) return current;
    return this.createSession();
  }

  /** 切换会话 */
  switchSession(sessionId: string): UnifiedSession | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      this.currentSessionId = sessionId;
      return session;
    }
    return null;
  }

  /** 获取会话 */
  getSession(sessionId: string): UnifiedSession | null {
    return this.sessions.get(sessionId) ?? null;
  }

  /** 获取所有会话（按更新时间倒序） */
  getAllSessions(): UnifiedSession[] {
    return Array.from(this.sessions.values())
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }

  /** 获取会话元数据列表 */
  getSessionMetas(): SessionMeta[] {
    return this.getAllSessions().map(s => ({
      id: s.id,
      name: s.name,
      messageCount: s.messages.length,
      createdAt: s.createdAt,
      updatedAt: s.updatedAt,
      preview: this.getSessionPreview(s),
    }));
  }

  /** 获取会话预览 */
  private getSessionPreview(session: UnifiedSession): string {
    const firstUserMsg = session.messages.find(m => m.role === 'user');
    if (!firstUserMsg) return '新对话';
    const content = firstUserMsg.content.trim();
    return content.length > 50 ? content.substring(0, 50) + '...' : content;
  }

  /** 获取当前会话 ID */
  getCurrentSessionId(): string | null {
    return this.currentSessionId;
  }

  // ============================================================================
  // 消息管理
  // ============================================================================

  /** 添加消息到当前会话 */
  addMessage(
    role: 'user' | 'assistant',
    content: string,
    cli?: CLIType,
    source?: 'orchestrator' | 'worker' | 'system'
  ): SessionMessage {
    const session = this.getOrCreateCurrentSession();
    const message: SessionMessage = {
      id: generateMessageId(),
      role,
      content,
      cli,
      source,
      timestamp: Date.now(),
    };

    session.messages.push(message);
    session.updatedAt = Date.now();

    // 自动生成会话标题
    if (!session.name && role === 'user' && session.messages.filter(m => m.role === 'user').length === 1) {
      session.name = this.generateSessionTitle(content);
    }

    this.saveSession(session);
    return message;
  }

  /** 生成会话标题 */
  private generateSessionTitle(firstMessage: string): string {
    let text = firstMessage.trim().replace(/\n+/g, ' ').replace(/\s+/g, ' ');

    // 移除冗余前缀
    const prefixes = [/^(请|帮我|帮忙|能不能|可以|麻烦|我想|我要|我需要)/, /^(please|can you|could you|help me)/i];
    for (const p of prefixes) text = text.replace(p, '').trim();

    // 移除末尾语气词
    const suffixes = [/(吗|呢|吧|啊|谢谢|thanks)[\s。？?！!]*$/i];
    for (const s of suffixes) text = text.replace(s, '').trim();

    return text.length <= 100 ? text : text.substring(0, 100) + '...';
  }

  /** 更新会话数据 */
  updateSessionData(sessionId: string, messages: SessionMessage[], cliOutputs?: Record<string, any[]>): boolean {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.messages = messages;
      if (cliOutputs) session.cliOutputs = cliOutputs;
      session.updatedAt = Date.now();
      this.saveSession(session);
      return true;
    }
    return false;
  }

  /** 重命名会话 */
  renameSession(sessionId: string, name: string): boolean {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.name = name;
      session.updatedAt = Date.now();
      this.saveSession(session);
      return true;
    }
    return false;
  }

  /** 清空当前会话消息 */
  clearCurrentSessionMessages(): void {
    const session = this.getCurrentSession();
    if (session) {
      session.messages = [];
      session.updatedAt = Date.now();
      this.saveSession(session);
    }
  }

  /** 获取最近消息 */
  getRecentMessages(count: number = 10): SessionMessage[] {
    const session = this.getCurrentSession();
    if (!session) return [];
    return session.messages.slice(-count);
  }

  // ============================================================================
  // Task 管理
  // ============================================================================

  /** 添加 Task 到会话 */
  addTask(sessionId: string, task: Task): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.tasks.push(task);
      session.updatedAt = Date.now();
      this.saveSession(session);
    }
  }

  /** 更新 Task */
  updateTask(sessionId: string, taskId: string, updates: Partial<Task>): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      const taskIndex = session.tasks.findIndex(t => t.id === taskId);
      if (taskIndex !== -1) {
        session.tasks[taskIndex] = { ...session.tasks[taskIndex], ...updates };
        session.updatedAt = Date.now();
        this.saveSession(session);
      }
    }
  }

  /** 获取会话的所有任务 */
  getTasks(sessionId: string): Task[] {
    const session = this.sessions.get(sessionId);
    return session?.tasks ?? [];
  }

  /** 清空会话的任务列表 */
  clearTasks(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.tasks = [];
      session.updatedAt = Date.now();
      this.saveSession(session);
    }
  }

  // ============================================================================
  // 快照管理
  // ============================================================================

  /** 添加快照元数据 */
  addSnapshot(sessionId: string, snapshot: FileSnapshotMeta): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      const existingIndex = session.snapshots.findIndex(s => s.filePath === snapshot.filePath);
      if (existingIndex !== -1) {
        session.snapshots[existingIndex] = snapshot;
      } else {
        session.snapshots.push(snapshot);
      }
      this.saveSession(session);
    }
  }

  /** 获取快照元数据 */
  getSnapshot(sessionId: string, filePath: string): FileSnapshotMeta | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      return session.snapshots.find(s => s.filePath === filePath) ?? null;
    }
    return null;
  }

  /** 移除快照元数据 */
  removeSnapshot(sessionId: string, filePath: string): boolean {
    const session = this.sessions.get(sessionId);
    if (session) {
      const index = session.snapshots.findIndex(s => s.filePath === filePath);
      if (index !== -1) {
        session.snapshots.splice(index, 1);
        this.saveSession(session);
        return true;
      }
    }
    return false;
  }

  /** 获取快照文件存储路径 */
  getSnapshotFilePath(sessionId: string, snapshotId: string): string {
    return path.join(this.getSessionDir(sessionId), 'snapshots', `${snapshotId}.snapshot`);
  }

  // ============================================================================
  // 会话删除（清理整个会话目录）
  // ============================================================================

  /** 删除会话（删除整个会话目录） */
  deleteSession(sessionId: string): boolean {
    const session = this.sessions.get(sessionId);
    if (!session) return false;

    // 从内存中移除
    this.sessions.delete(sessionId);

    // 删除整个会话目录
    const sessionDir = this.getSessionDir(sessionId);
    if (fs.existsSync(sessionDir)) {
      fs.rmSync(sessionDir, { recursive: true, force: true });
    }

    console.log(`[UnifiedSessionManager] 已删除会话: ${sessionId}`);

    // 如果删除的是当前会话，切换到最新的会话
    if (this.currentSessionId === sessionId) {
      const sessions = this.getAllSessions();
      this.currentSessionId = sessions.length > 0 ? sessions[0].id : null;
    }

    globalEventBus.emitEvent('session:ended', { sessionId });
    return true;
  }

  /** 结束会话（标记为完成但不删除） */
  endSession(sessionId: string): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.status = 'completed';
      this.saveSession(session);
      if (this.currentSessionId === sessionId) {
        this.currentSessionId = null;
      }
    }
  }

  // ============================================================================
  // 持久化
  // ============================================================================

  /** 保存会话 */
  saveSession(session: UnifiedSession): void {
    this.ensureSessionDir(session.id);
    const filePath = this.getSessionFilePath(session.id);
    fs.writeFileSync(filePath, JSON.stringify(session, null, 2), 'utf-8');
  }

  /** 保存当前会话 */
  saveCurrentSession(): void {
    const session = this.getCurrentSession();
    if (session) {
      this.saveSession(session);
    }
  }

  /** 加载会话 */
  private loadSession(sessionId: string): UnifiedSession | null {
    const filePath = this.getSessionFilePath(sessionId);
    if (fs.existsSync(filePath)) {
      try {
        const data = fs.readFileSync(filePath, 'utf-8');
        const session = JSON.parse(data) as UnifiedSession;
        this.sessions.set(session.id, session);
        return session;
      } catch (e) {
        console.error(`[UnifiedSessionManager] 加载会话失败: ${sessionId}`, e);
      }
    }
    return null;
  }

  /** 加载所有会话 */
  private loadAllSessions(): void {
    if (!fs.existsSync(this.baseDir)) return;

    // 遍历 sessions 目录下的所有子目录
    const entries = fs.readdirSync(this.baseDir, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.isDirectory()) {
        const sessionId = entry.name;
        this.loadSession(sessionId);
      }
    }

    // 设置当前会话为最新的会话
    const sessions = this.getAllSessions();
    if (sessions.length > 0) {
      this.currentSessionId = sessions[0].id;
    }
  }

  // ============================================================================
  // 辅助路径方法（供其他管理器使用）
  // ============================================================================

  /** 获取计划目录 */
  getPlansDir(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'plans');
  }

  /** 获取任务状态文件路径 */
  getTasksFilePath(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'tasks.json');
  }

  /** 获取执行状态文件路径 */
  getExecutionStateFilePath(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'execution-state.json');
  }

  /** 获取快照目录 */
  getSnapshotsDir(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'snapshots');
  }

  // ============================================================================
  // 格式化和清理方法
  // ============================================================================

  /** 格式化对话历史为字符串（用于 Prompt 增强） */
  formatConversationHistory(count: number = 10): string {
    const messages = this.getRecentMessages(count);
    if (messages.length === 0) {
      return '';
    }
    return messages
      .map(m => `${m.role === 'user' ? 'User' : 'Assistant'}: ${m.content}`)
      .join('\n\n');
  }

  /** 清理任务状态文件（删除会话时自动清理，因为在同一目录） */
  private cleanupTaskState(sessionId: string): void {
    const taskFilePath = this.getTasksFilePath(sessionId);
    if (fs.existsSync(taskFilePath)) {
      try {
        fs.unlinkSync(taskFilePath);
        console.log(`[UnifiedSessionManager] 已清理任务状态: ${taskFilePath}`);
      } catch (e) {
        console.error(`[UnifiedSessionManager] 清理任务状态失败: ${taskFilePath}`, e);
      }
    }
  }

  /** 清理图片附件 */
  private cleanupAttachments(session: UnifiedSession): void {
    for (const message of session.messages) {
      if (message.attachments && message.attachments.length > 0) {
        for (const attachment of message.attachments) {
          if (attachment.path.includes('.multicli/attachments') && fs.existsSync(attachment.path)) {
            try {
              fs.unlinkSync(attachment.path);
              console.log(`[UnifiedSessionManager] 已清理图片附件: ${attachment.path}`);
            } catch (e) {
              console.error(`[UnifiedSessionManager] 清理图片附件失败: ${attachment.path}`, e);
            }
          }
        }
      }
    }
  }
}

