/**
 * 对话会话管理器
 * 管理对话历史、会话切换、持久化
 */

import * as fs from 'fs';
import * as path from 'path';
import { CLIType } from './types';
import { Session, SessionMessage, SessionMeta } from './cli/types';

/** 生成唯一 ID */
function generateId(): string {
  return `chat-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/** 生成消息 ID */
function generateMessageId(): string {
  return `msg-${Date.now()}-${Math.random().toString(36).substring(2, 6)}`;
}

/**
 * 对话会话管理器
 * 专门管理聊天对话历史
 */
export class ChatSessionManager {
  private sessions: Map<string, Session> = new Map();
  private currentSessionId: string | null = null;
  private storageDir: string;
  private workspaceRoot: string;

  constructor(workspaceRoot: string) {
    this.workspaceRoot = workspaceRoot;
    this.storageDir = path.join(workspaceRoot, '.multicli', 'chat-sessions');
    this.ensureStorageDir();
    this.loadAllSessions();
  }

  /** 确保存储目录存在 */
  private ensureStorageDir(): void {
    if (!fs.existsSync(this.storageDir)) {
      fs.mkdirSync(this.storageDir, { recursive: true });
    }
  }

  /** 创建新会话 */
  createSession(name?: string, sessionId?: string): Session {
    if (sessionId && this.sessions.has(sessionId)) {
      this.currentSessionId = sessionId;
      return this.sessions.get(sessionId)!;
    }

    const now = Date.now();
    const session: Session = {
      id: sessionId ?? generateId(),
      name: name || undefined,
      messages: [],
      createdAt: now,
      updatedAt: now,
    };

    this.sessions.set(session.id, session);
    this.currentSessionId = session.id;
    this.saveSession(session);

    return session;
  }

  /** 获取当前会话 */
  getCurrentSession(): Session | null {
    if (!this.currentSessionId) return null;
    return this.sessions.get(this.currentSessionId) ?? null;
  }

  /** 获取或创建当前会话 */
  getOrCreateCurrentSession(): Session {
    const current = this.getCurrentSession();
    if (current) return current;
    return this.createSession();
  }

  /** 切换会话 */
  switchSession(sessionId: string): Session | null {
    const session = this.sessions.get(sessionId);
    if (session) {
      this.currentSessionId = sessionId;
      return session;
    }
    return null;
  }

  /** 获取会话 */
  getSession(sessionId: string): Session | null {
    return this.sessions.get(sessionId) ?? null;
  }

  /** 获取所有会话（按更新时间倒序） */
  getAllSessions(): Session[] {
    return Array.from(this.sessions.values())
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }

  /** 获取会话元数据列表（用于 UI 显示） */
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

  /** 获取会话预览（第一条用户消息的前 50 个字符） */
  private getSessionPreview(session: Session): string {
    const firstUserMsg = session.messages.find(m => m.role === 'user');
    if (!firstUserMsg) return '新对话';
    const content = firstUserMsg.content.trim();
    return content.length > 50 ? content.substring(0, 50) + '...' : content;
  }

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

    // 自动生成会话标题（基于第一条用户消息）
    if (!session.name && role === 'user' && session.messages.filter(m => m.role === 'user').length === 1) {
      session.name = this.generateSessionTitle(content);
    }

    this.saveSession(session);
    return message;
  }

  /**
   * 生成会话标题 - 参考 Augment 风格的智能命名
   * 1. 移除冗余词汇（帮我、请、能不能等）
   * 2. 提取关键动词+对象
   * 3. 识别代码相关名称（函数名、文件名等）
   * 4. 智能截断，不切断单词
   */
  private generateSessionTitle(firstMessage: string): string {
    // 清理：移除换行，合并空格
    let text = firstMessage.trim().replace(/\n+/g, ' ').replace(/\s+/g, ' ');

    // 移除常见的冗余前缀词
    const redundantPrefixes = [
      /^(请|帮我|帮忙|能不能|可以|麻烦|我想|我要|我需要|希望你|你能|你可以)/,
      /^(please|can you|could you|would you|i want to|i need to|help me)/i,
    ];
    for (const pattern of redundantPrefixes) {
      text = text.replace(pattern, '').trim();
    }

    // 移除末尾的语气词
    const redundantSuffixes = [
      /(吗|呢|吧|啊|哦|呀|嘛|谢谢|thanks|thank you)[\s。？?！!]*$/i,
    ];
    for (const pattern of redundantSuffixes) {
      text = text.replace(pattern, '').trim();
    }

    // 直接截断原文（基于第一条用户消息）
    const maxLength = 100;
    if (text.length <= maxLength) {
      return text;
    }
    return text.substring(0, maxLength) + '...';
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

  /** 更新会话数据（从前端同步） */
  updateSessionData(sessionId: string, messages: SessionMessage[], cliOutputs?: Record<string, any[]>): boolean {
    const session = this.sessions.get(sessionId);
    if (session) {
      session.messages = messages;
      if (cliOutputs) {
        (session as any).cliOutputs = cliOutputs;
      }
      session.updatedAt = Date.now();
      this.saveSession(session);
      return true;
    }
    return false;
  }

  /** 保存当前会话 */
  saveCurrentSession(): void {
    const session = this.getCurrentSession();
    if (session) {
      this.saveSession(session);
    }
  }

  /** 删除会话（同时清理相关资源） */
  deleteSession(sessionId: string): boolean {
    const session = this.sessions.get(sessionId);
    if (!session) return false;

    // 1. 删除会话文件
    this.sessions.delete(sessionId);
    const filePath = path.join(this.storageDir, `${sessionId}.json`);
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
    }

    // 2. 清理关联的任务状态文件
    this.cleanupTaskState(sessionId);

    // 3. 清理 Memory 文档
    this.cleanupMemoryDocument(sessionId);

    // 4. 清理图片附件（如果有）
    this.cleanupAttachments(session);

    console.log(`[ChatSessionManager] 已删除会话及相关资源: ${sessionId}`);

    // 如果删除的是当前会话，切换到最新的会话或创建新会话
    if (this.currentSessionId === sessionId) {
      const sessions = this.getAllSessions();
      this.currentSessionId = sessions.length > 0 ? sessions[0].id : null;
    }

    return true;
  }

  /** 清理任务状态文件 */
  private cleanupTaskState(sessionId: string): void {
    const taskFilePath = path.join(this.workspaceRoot, '.multicli', 'tasks', `${sessionId}.json`);
    if (fs.existsSync(taskFilePath)) {
      try {
        fs.unlinkSync(taskFilePath);
        console.log(`[ChatSessionManager] 已清理任务状态: ${taskFilePath}`);
      } catch (e) {
        console.error(`[ChatSessionManager] 清理任务状态失败: ${taskFilePath}`, e);
      }
    }
  }

  /** 清理 Memory 文档 */
  private cleanupMemoryDocument(sessionId: string): void {
    const memoryDir = path.join(this.workspaceRoot, '.multicli', 'sessions', sessionId);
    if (fs.existsSync(memoryDir)) {
      try {
        // 递归删除目录
        this.removeDirectoryRecursive(memoryDir);
        console.log(`[ChatSessionManager] 已清理 Memory 文档: ${memoryDir}`);
      } catch (e) {
        console.error(`[ChatSessionManager] 清理 Memory 文档失败: ${memoryDir}`, e);
      }
    }
  }

  /** 清理图片附件 */
  private cleanupAttachments(session: Session): void {
    // 遍历会话消息，查找图片附件
    for (const message of session.messages) {
      if (message.attachments && message.attachments.length > 0) {
        for (const attachment of message.attachments) {
          // 只清理工作区内的临时图片（.multicli/attachments 目录下的）
          if (attachment.path.includes('.multicli/attachments') && fs.existsSync(attachment.path)) {
            try {
              fs.unlinkSync(attachment.path);
              console.log(`[ChatSessionManager] 已清理图片附件: ${attachment.path}`);
            } catch (e) {
              console.error(`[ChatSessionManager] 清理图片附件失败: ${attachment.path}`, e);
            }
          }
        }
      }
    }
  }

  /** 递归删除目录 */
  private removeDirectoryRecursive(dirPath: string): void {
    if (fs.existsSync(dirPath)) {
      const files = fs.readdirSync(dirPath);
      for (const file of files) {
        const curPath = path.join(dirPath, file);
        if (fs.lstatSync(curPath).isDirectory()) {
          this.removeDirectoryRecursive(curPath);
        } else {
          fs.unlinkSync(curPath);
        }
      }
      fs.rmdirSync(dirPath);
    }
  }

  /** 保存会话到文件 */
  private saveSession(session: Session): void {
    const filePath = path.join(this.storageDir, `${session.id}.json`);
    fs.writeFileSync(filePath, JSON.stringify(session, null, 2), 'utf-8');
  }

  /** 从文件加载会话 */
  private loadSession(sessionId: string): Session | null {
    const filePath = path.join(this.storageDir, `${sessionId}.json`);
    if (fs.existsSync(filePath)) {
      try {
        const data = fs.readFileSync(filePath, 'utf-8');
        const session = JSON.parse(data) as Session;
        this.sessions.set(session.id, session);
        return session;
      } catch (e) {
        console.error(`[ChatSessionManager] 加载会话失败: ${sessionId}`, e);
      }
    }
    return null;
  }

  /** 加载所有会话 */
  private loadAllSessions(): void {
    if (!fs.existsSync(this.storageDir)) return;
    const files = fs.readdirSync(this.storageDir).filter(f => f.endsWith('.json'));
    for (const file of files) {
      const sessionId = file.replace('.json', '');
      this.loadSession(sessionId);
    }

    // 设置当前会话为最新的会话
    const sessions = this.getAllSessions();
    if (sessions.length > 0) {
      this.currentSessionId = sessions[0].id;
    }
  }

  /** 获取当前会话 ID */
  getCurrentSessionId(): string | null {
    return this.currentSessionId;
  }

  /** 清空当前会话的消息（但保留会话） */
  clearCurrentSessionMessages(): void {
    const session = this.getCurrentSession();
    if (session) {
      session.messages = [];
      session.updatedAt = Date.now();
      this.saveSession(session);
    }
  }

  /** 获取最近 N 条消息（用于 Prompt 增强上下文） */
  getRecentMessages(count: number = 10): SessionMessage[] {
    const session = this.getCurrentSession();
    if (!session || session.messages.length === 0) {
      return [];
    }
    return session.messages.slice(-count);
  }

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
}
