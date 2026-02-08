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

import { logger, LogCategory } from '../logging';
import * as fs from 'fs';
import * as path from 'path';
import { FileSnapshot } from '../types';
import { AgentType } from '../types/agent-types';
import { globalEventBus } from '../events';

/** 会话消息 */
export interface SessionMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  agent?: AgentType;
  source?: 'orchestrator' | 'worker' | 'system';
  timestamp: number;
  attachments?: { name: string; path: string; mimeType?: string }[];
  /** 用户上传的图片（base64 Data URL 格式） */
  images?: Array<{ dataUrl: string }>;
}

/** 文件快照元数据 */
export interface FileSnapshotMeta {
  id: string;
  filePath: string;
  timestamp: number;

  // Mission 架构字段
  missionId: string;
  assignmentId: string;
  todoId: string;
  workerId: string;  // Worker 标识（claude/codex/gemini）
  contributors?: string[];

  agentType?: AgentType;
  reason?: string;
}

/** 任务状态 */
export type SessionStatus = 'active' | 'completed';

/** 会话总结（用于会话恢复） */
export interface SessionSummary {
  sessionId: string;
  title: string;
  objective: string;              // 会话目标/主题
  completedTasks: string[];       // 已完成任务摘要
  inProgressTasks: string[];      // 进行中任务摘要
  keyDecisions: string[];         // 关键决策
  codeChanges: string[];          // 代码变更摘要
  pendingIssues: string[];        // 待解决问题
  messageCount: number;           // 消息数量
  lastUpdated: number;            // 最后更新时间
}

/** 统一会话数据结构
 *
 * 注意：任务管理已迁移到 Mission 系统
 * 使用 MissionDrivenEngine.listTaskViews() 获取任务列表
 */
export interface UnifiedSession {
  id: string;
  name?: string;
  status: SessionStatus;
  createdAt: number;
  updatedAt: number;
  /** 聊天消息 */
  messages: SessionMessage[];
  /** 快照元数据 */
  snapshots: FileSnapshotMeta[];
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

  // 内存管理配置
  private readonly MAX_SESSIONS_IN_MEMORY = 50;  // 最大内存中会话数
  private readonly MAX_MESSAGES_PER_SESSION = 1000;  // 每个会话最大消息数
  private readonly MESSAGE_CLEANUP_THRESHOLD = 800;  // 消息清理阈值

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
      snapshots: [],
    };

    this.ensureSessionDir(id);

    // 内存管理：如果会话数超过限制，驱逐最早的非当前会话
    this.evictOldSessionsIfNeeded();

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
      messageCount: s.messages.filter(m => m.role === 'user').length,
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
    agent?: AgentType,  // ✅ 使用 AgentType
    source?: 'orchestrator' | 'worker' | 'system',
    images?: Array<{ dataUrl: string }>  // 🔧 新增：用户上传的图片
  ): SessionMessage {
    const session = this.getOrCreateCurrentSession();
    const message: SessionMessage = {
      id: generateMessageId(),
      role,
      content,
      agent,  // ✅ 使用 agent
      source,
      timestamp: Date.now(),
      images: images && images.length > 0 ? images : undefined,  // 🔧 保存图片
    };

    session.messages.push(message);
    session.updatedAt = Date.now();

    // 自动生成会话标题
    if (!session.name && role === 'user' && session.messages.filter(m => m.role === 'user').length === 1) {
      session.name = this.generateSessionTitle(content);
    }

    // 消息数量管理：如果超过阈值，清理历史消息
    this.cleanupOldMessagesIfNeeded(session);

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
  updateSessionData(sessionId: string, messages: SessionMessage[]): boolean {  // ✅ 移除 cliOutputs 参数
    const session = this.sessions.get(sessionId);
    if (session) {
      for (const msg of messages) {
        if (!msg.id || typeof msg.id !== 'string' || !msg.id.trim()) {
          throw new Error('Session message missing id');
        }
        if (!msg.role || !['user', 'assistant', 'system'].includes(msg.role)) {
          throw new Error('Session message role invalid');
        }
        if (typeof msg.content !== 'string') {
          throw new Error('Session message content invalid');
        }
        if (typeof msg.timestamp !== 'number') {
          throw new Error('Session message timestamp invalid');
        }
      }
      session.messages = messages;
      // ✅ 移除 cliOutputs 更新逻辑
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

  /** 估算消息的 token 数量（粗略估算：1 token ≈ 4 字符） */
  private estimateTokenCount(text: string): number {
    // 简单估算：英文约 4 字符/token，中文约 1.5 字符/token
    // 使用保守估算：平均 3 字符/token
    return Math.ceil(text.length / 3);
  }

  /** 获取消息的总 token 数 */
  private getMessageTokenCount(message: SessionMessage): number {
    let total = this.estimateTokenCount(message.content);

    // 添加元数据的 token 开销（role, timestamp 等）
    total += 20; // 固定开销

    return total;
  }

  /** 获取在 token 预算内的最近消息 */
  getRecentMessagesWithinTokenBudget(maxTokens: number = 8000): SessionMessage[] {
    const session = this.getCurrentSession();
    if (!session || session.messages.length === 0) return [];

    const messages: SessionMessage[] = [];
    let totalTokens = 0;

    // 从最新消息开始，向前累加
    for (let i = session.messages.length - 1; i >= 0; i--) {
      const message = session.messages[i];
      const messageTokens = this.getMessageTokenCount(message);

      if (totalTokens + messageTokens > maxTokens) {
        // 超出预算，停止添加
        break;
      }

      messages.unshift(message); // 添加到开头以保持顺序
      totalTokens += messageTokens;
    }

    return messages;
  }

  /** 获取上下文窗口统计信息 */
  getContextWindowStats(): {
    totalMessages: number;
    estimatedTokens: number;
    oldestMessageAge: number;
    newestMessageAge: number;
  } {
    const session = this.getCurrentSession();
    if (!session || session.messages.length === 0) {
      return {
        totalMessages: 0,
        estimatedTokens: 0,
        oldestMessageAge: 0,
        newestMessageAge: 0,
      };
    }

    const now = Date.now();
    let totalTokens = 0;

    for (const message of session.messages) {
      totalTokens += this.getMessageTokenCount(message);
    }

    return {
      totalMessages: session.messages.length,
      estimatedTokens: totalTokens,
      oldestMessageAge: now - session.messages[0].timestamp,
      newestMessageAge: now - session.messages[session.messages.length - 1].timestamp,
    };
  }

  // ============================================================================
  // 快照管理
  // ============================================================================

  /** 添加快照元数据 */
  addSnapshot(sessionId: string, snapshot: FileSnapshotMeta): void {
    const session = this.sessions.get(sessionId);
    if (session) {
      if (!this.isValidSnapshotMeta(snapshot)) {
        logger.error('会话.快照.非法_元数据', { sessionId, snapshot }, LogCategory.SESSION);
        throw new Error('Invalid snapshot metadata');
      }
      const existingIndex = session.snapshots.findIndex(s => s.filePath === snapshot.filePath);
      if (existingIndex !== -1) {
        const previous = session.snapshots[existingIndex];
        const previousContributors = previous.contributors ?? [previous.workerId];
        const nextContributors = snapshot.contributors ?? [snapshot.workerId];
        snapshot.contributors = Array.from(new Set([...previousContributors, ...nextContributors]));
        session.snapshots[existingIndex] = snapshot;
        if (previous.id !== snapshot.id) {
          const oldFile = this.getSnapshotFilePath(sessionId, previous.id);
          if (fs.existsSync(oldFile)) {
            try {
              fs.unlinkSync(oldFile);
            } catch (error) {
              logger.warn('会话.快照.清理_失败', { oldFile, error }, LogCategory.SESSION);
              // 不抛出错误，继续执行
            }
          }
        }
      } else {
        session.snapshots.push(snapshot);
      }

      try {
        this.saveSession(session);
      } catch (error) {
        logger.error('会话.快照.保存_失败', error, LogCategory.SESSION);
        throw error;
      }
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
      try {
        fs.rmSync(sessionDir, { recursive: true, force: true });
        logger.info('会话.删除.成功', { sessionId }, LogCategory.SESSION);
      } catch (error) {
        logger.error('会话.删除.失败', { sessionId, error }, LogCategory.SESSION);
        // 即使删除失败，也从内存中移除了，返回 true
        // 用户可以手动清理文件系统
      }
    }

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
  // 数据完整性验证
  // ============================================================================

  /** 验证会话数据完整性 */
  private validateSessionData(session: any): boolean {
    // 基础字段验证
    if (!session || typeof session !== 'object') {
      return false;
    }

    // 必需字段验证
    if (!session.id || typeof session.id !== 'string') {
      logger.error('会话.验证.缺失标识', undefined, LogCategory.SESSION);
      return false;
    }

    if (!session.status || !['active', 'completed'].includes(session.status)) {
      logger.error('会话.验证.非法_状态', { status: session.status }, LogCategory.SESSION);
      return false;
    }

    if (typeof session.createdAt !== 'number' || typeof session.updatedAt !== 'number') {
      logger.error('会话.验证.非法_时间戳', undefined, LogCategory.SESSION);
      return false;
    }

    // 数组字段验证
    if (!Array.isArray(session.messages)) {
      logger.error('会话.验证.消息_非数组', undefined, LogCategory.SESSION);
      return false;
    }

    if (!Array.isArray(session.snapshots)) {
      logger.error('会话.验证.快照_非数组', undefined, LogCategory.SESSION);
      return false;
    }

    // 消息数据验证
    for (const msg of session.messages) {
      if (!msg.id || typeof msg.id !== 'string' || !msg.id.trim()) {
        logger.error('会话.验证.消息_缺失_id', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (!msg.role || !['user', 'assistant', 'system'].includes(msg.role)) {
        logger.error('会话.验证.消息_非法_role', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (typeof msg.content !== 'string') {
        logger.error('会话.验证.消息_非法_content', { message: msg }, LogCategory.SESSION);
        return false;
      }
      if (typeof msg.timestamp !== 'number') {
        logger.error('会话.验证.消息_非法_timestamp', { message: msg }, LogCategory.SESSION);
        return false;
      }
    }

    return true;
  }

  private isValidSnapshotMeta(snapshot: FileSnapshotMeta | undefined | null): snapshot is FileSnapshotMeta {
    if (!snapshot || typeof snapshot !== 'object') return false;
    if (!snapshot.id || typeof snapshot.id !== 'string') return false;
    if (!snapshot.filePath || typeof snapshot.filePath !== 'string' || snapshot.filePath.trim().length === 0) {
      return false;
    }
    if (typeof snapshot.timestamp !== 'number') return false;
    if (!snapshot.workerId || typeof snapshot.workerId !== 'string') return false;
    if (!snapshot.missionId || typeof snapshot.missionId !== 'string') return false;
    if (!snapshot.assignmentId || typeof snapshot.assignmentId !== 'string') return false;
    if (!snapshot.todoId || typeof snapshot.todoId !== 'string') return false;
    return true;
  }

  /** 备份损坏的会话文件 */
  private backupCorruptedSession(sessionId: string, filePath: string): void {
    try {
      const backupPath = `${filePath}.corrupted.${Date.now()}.bak`;
      if (fs.existsSync(filePath)) {
        fs.copyFileSync(filePath, backupPath);
        logger.info('会话.备份.损坏.成功', { backupPath }, LogCategory.SESSION);
      }
    } catch (error) {
      logger.error('会话.备份.损坏.失败', { sessionId, error }, LogCategory.SESSION);
    }
  }

  // ============================================================================
  // 内存管理
  // ============================================================================

  /** 驱逐历史会话（如果超过内存限制） */
  private evictOldSessionsIfNeeded(): void {
    if (this.sessions.size <= this.MAX_SESSIONS_IN_MEMORY) {
      return;
    }

    // 获取所有会话，按更新时间排序（最早的在前）
    const allSessions = Array.from(this.sessions.values())
      .sort((a, b) => a.updatedAt - b.updatedAt);

    // 计算需要驱逐的会话数
    const toEvict = this.sessions.size - this.MAX_SESSIONS_IN_MEMORY;

    // 驱逐最早的非当前会话
    let evicted = 0;
    for (const session of allSessions) {
      if (evicted >= toEvict) break;
      if (session.id === this.currentSessionId) continue; // 不驱逐当前会话

      // 保存到磁盘后从内存中移除
      this.saveSession(session);
      this.sessions.delete(session.id);
      evicted++;
    }

    if (evicted > 0) {
      logger.info('会话.清理.完成', { count: evicted }, LogCategory.SESSION);
    }
  }

  /** 清理历史消息（如果超过阈值） */
  private cleanupOldMessagesIfNeeded(session: UnifiedSession): void {
    if (session.messages.length <= this.MESSAGE_CLEANUP_THRESHOLD) {
      return;
    }

    // 保留最近的消息，删除最早的消息
    const toKeep = Math.floor(this.MAX_MESSAGES_PER_SESSION * 0.8); // 保留 80%
    const removed = session.messages.length - toKeep;

    logger.info(
      '会话.消息.清理',
      { sessionId: session.id, total: session.messages.length, threshold: this.MESSAGE_CLEANUP_THRESHOLD, removed },
      LogCategory.SESSION
    );

    session.messages = session.messages.slice(-toKeep);
  }

  // ============================================================================
  // 持久化
  // ============================================================================

  /** 保存会话 */
  saveSession(session: UnifiedSession): void {
    this.ensureSessionDir(session.id);
    const filePath = this.getSessionFilePath(session.id);
    try {
      fs.writeFileSync(filePath, JSON.stringify(session, null, 2), 'utf-8');
    } catch (error) {
      logger.error('会话.保存.失败', { sessionId: session.id, error }, LogCategory.SESSION);
      throw new Error(`Failed to save session: ${error}`);
    }
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
        let session = JSON.parse(data) as UnifiedSession;

        // 数据完整性验证
        if (!this.validateSessionData(session)) {
          logger.error('会话.加载.校验_失败', { sessionId }, LogCategory.SESSION);
          this.backupCorruptedSession(sessionId, filePath);
          return null;
        }

        this.sessions.set(session.id, session);
        return session;
      } catch (e) {
        logger.error('会话.加载.失败', { sessionId, error: e }, LogCategory.SESSION);
        // 尝试备份损坏的会话文件
        this.backupCorruptedSession(sessionId, filePath);
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
  // 会话总结生成（用于会话恢复）
  // ============================================================================

  /** 生成会话总结（用于会话切换时的上下文注入）
   * 注意：任务信息现在从 Mission 系统获取，此方法返回的任务信息可能为空
   * 调用方应使用 MissionDrivenEngine.listTaskViews() 获取完整任务列表
   */
  getSessionSummary(sessionId?: string): SessionSummary | null {
    const session = sessionId ? this.sessions.get(sessionId) : this.getCurrentSession();
    if (!session) return null;

    // 任务信息已迁移到 Mission 系统，这里返回空数组
    // 调用方应使用 MissionDrivenEngine.listTaskViews() 获取任务
    const completedTasks: string[] = [];
    const inProgressTasks: string[] = [];
    const pendingIssues: string[] = [];

    // 提取代码变更摘要
    const codeChanges = session.snapshots
      .map(s => `${s.filePath} (${s.workerId})`)
      .slice(0, 20); // 最多 20 个文件

    // 提取关键决策（从消息中提取）
    const keyDecisions = this.extractKeyDecisions(session.messages);

    // 生成会话目标（从第一条用户消息或会话名称）
    const objective = this.extractObjective(session);

    return {
      sessionId: session.id,
      title: session.name || '未命名会话',
      objective,
      completedTasks,
      inProgressTasks,
      keyDecisions,
      codeChanges,
      pendingIssues,
      messageCount: session.messages.filter(m => m.role === 'user').length,
      lastUpdated: session.updatedAt,
    };
  }

  /** 提取会话目标 */
  private extractObjective(session: UnifiedSession): string {
    // 优先使用会话名称
    if (session.name) {
      return session.name;
    }

    // 否则使用第一条用户消息
    const firstUserMsg = session.messages.find(m => m.role === 'user');
    if (firstUserMsg) {
      const content = firstUserMsg.content.trim();
      return content.length > 100 ? content.substring(0, 100) + '...' : content;
    }

    return '新对话';
  }

  /** 提取关键决策（简单规则：包含关键词的消息） */
  private extractKeyDecisions(messages: SessionMessage[]): string[] {
    const decisionKeywords = [
      '决定', '选择', '采用', '使用', '方案', '架构',
      'decide', 'choose', 'use', 'adopt', 'approach', 'architecture'
    ];

    const decisions: string[] = [];

    for (const msg of messages) {
      if (msg.role !== 'assistant') continue;

      const content = msg.content.toLowerCase();
      const hasKeyword = decisionKeywords.some(kw => content.includes(kw.toLowerCase()));

      if (hasKeyword) {
        // 提取包含关键词的句子
        const sentences = msg.content.split(/[。！？.!?]/);
        for (const sentence of sentences) {
          const sentenceLower = sentence.toLowerCase();
          if (decisionKeywords.some(kw => sentenceLower.includes(kw.toLowerCase()))) {
            const trimmed = sentence.trim();
            if (trimmed.length > 10 && trimmed.length < 200) {
              decisions.push(trimmed);
              if (decisions.length >= 5) break; // 最多 5 个决策
            }
          }
        }
      }

      if (decisions.length >= 5) break;
    }

    return decisions;
  }

  /** 格式化会话总结为文本（用于注入到上下文） */
  formatSessionSummary(summary: SessionSummary): string {
    const parts: string[] = [];

    parts.push(`# 会话总结: ${summary.title}`);
    parts.push(`会话目标: ${summary.objective}`);
    parts.push(`消息数量: ${summary.messageCount} 条`);
    parts.push('');

    if (summary.completedTasks.length > 0) {
      parts.push('## 已完成任务:');
      summary.completedTasks.forEach((task, i) => {
        parts.push(`${i + 1}. ${task}`);
      });
      parts.push('');
    }

    if (summary.inProgressTasks.length > 0) {
      parts.push('## 进行中任务:');
      summary.inProgressTasks.forEach((task, i) => {
        parts.push(`${i + 1}. ${task}`);
      });
      parts.push('');
    }

    if (summary.keyDecisions.length > 0) {
      parts.push('## 关键决策:');
      summary.keyDecisions.forEach((decision, i) => {
        parts.push(`${i + 1}. ${decision}`);
      });
      parts.push('');
    }

    if (summary.codeChanges.length > 0) {
      parts.push('## 代码变更:');
      summary.codeChanges.forEach((change, i) => {
        parts.push(`${i + 1}. ${change}`);
      });
      parts.push('');
    }

    if (summary.pendingIssues.length > 0) {
      parts.push('## 待解决问题:');
      summary.pendingIssues.forEach((issue, i) => {
        parts.push(`${i + 1}. ${issue}`);
      });
      parts.push('');
    }

    return parts.join('\n');
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
        logger.info('会话.清理.任务_状态.成功', { path: taskFilePath }, LogCategory.SESSION);
      } catch (e) {
        logger.error('会话.清理.任务_状态.失败', { path: taskFilePath, error: e }, LogCategory.SESSION);
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
              logger.info('会话.清理.附件.成功', { path: attachment.path }, LogCategory.SESSION);
            } catch (e) {
              logger.error('会话.清理.附件.失败', { path: attachment.path, error: e }, LogCategory.SESSION);
            }
          }
        }
      }
    }
  }

  // ============================================================================
  // Mission Storage 支持（新架构）
  // ============================================================================

  /** 获取会话的 missions 目录路径 */
  getMissionsDir(sessionId: string): string {
    return path.join(this.getSessionDir(sessionId), 'missions');
  }

  /** 确保 missions 目录存在 */
  ensureMissionsDir(sessionId: string): void {
    const missionsDir = this.getMissionsDir(sessionId);
    if (!fs.existsSync(missionsDir)) {
      fs.mkdirSync(missionsDir, { recursive: true });
    }
  }
}
