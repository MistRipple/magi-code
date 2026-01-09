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

  constructor(workspaceRoot: string) {
    this.storageDir = path.join(workspaceRoot, '.cli-arranger', 'chat-sessions');
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
  createSession(name?: string): Session {
    const now = Date.now();
    const session: Session = {
      id: generateId(),
      name: name || undefined,
      messages: [],
      createdAt: now,
      updatedAt: now,
      cliSessionIds: {},
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
  addMessage(role: 'user' | 'assistant', content: string, cli?: CLIType): SessionMessage {
    const session = this.getOrCreateCurrentSession();
    const message: SessionMessage = {
      id: generateMessageId(),
      role,
      content,
      cli,
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

    // 提取代码相关名称（函数名、文件名、类名等）
    const codePatterns = [
      /`([^`]+)`/,                           // 反引号包裹的代码
      /(\w+\.\w+)/,                          // 文件名 (xxx.ts)
      /(\w+(?:Service|Manager|Controller|Handler|Component|Module|Utils?))/i,  // 常见类名
      /(?:函数|方法|function|method)\s*[`'"]*(\w+)/i,  // 函数名
    ];

    let codeName = '';
    for (const pattern of codePatterns) {
      const match = text.match(pattern);
      if (match && match[1]) {
        codeName = match[1];
        break;
      }
    }

    // 提取动作关键词
    const actionKeywords = [
      { pattern: /(添加|新增|创建|实现|add|create|implement)/i, action: '添加' },
      { pattern: /(修复|修改|fix|repair|debug)/i, action: '修复' },
      { pattern: /(优化|改进|improve|optimize)/i, action: '优化' },
      { pattern: /(重构|refactor)/i, action: '重构' },
      { pattern: /(删除|移除|remove|delete)/i, action: '删除' },
      { pattern: /(更新|update)/i, action: '更新' },
      { pattern: /(测试|test)/i, action: '测试' },
      { pattern: /(分析|analyze|review)/i, action: '分析' },
      { pattern: /(解释|explain)/i, action: '解释' },
    ];

    let action = '';
    for (const { pattern, action: act } of actionKeywords) {
      if (pattern.test(text)) {
        action = act;
        break;
      }
    }

    // 如果有动作和代码名称，生成简洁标题
    if (action && codeName) {
      return `${action} ${codeName}`;
    }

    // 否则智能截断原文
    const maxLength = 25;
    if (text.length <= maxLength) {
      return text;
    }

    // 在词边界截断（中文按字符，英文按空格）
    const hasChinese = /[\u4e00-\u9fa5]/.test(text);
    if (hasChinese) {
      // 中文：直接截断
      return text.substring(0, maxLength) + '...';
    } else {
      // 英文：在空格处截断
      const truncated = text.substring(0, maxLength);
      const lastSpace = truncated.lastIndexOf(' ');
      if (lastSpace > maxLength * 0.6) {
        return truncated.substring(0, lastSpace) + '...';
      }
      return truncated + '...';
    }
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

  /** 更新 CLI 会话 ID */
  updateCliSessionId(cli: CLIType, cliSessionId: string | null): void {
    const session = this.getOrCreateCurrentSession();
    if (!session.cliSessionIds) {
      session.cliSessionIds = {};
    }
    if (cliSessionId) {
      session.cliSessionIds[cli] = cliSessionId;
    } else {
      delete session.cliSessionIds[cli];
    }
    this.saveSession(session);
  }

  /** 获取 CLI 会话 ID */
  getCliSessionId(cli: CLIType): string | undefined {
    const session = this.getCurrentSession();
    return session?.cliSessionIds?.[cli];
  }

  /** 删除会话 */
  deleteSession(sessionId: string): boolean {
    const session = this.sessions.get(sessionId);
    if (!session) return false;

    this.sessions.delete(sessionId);
    const filePath = path.join(this.storageDir, `${sessionId}.json`);
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
    }

    // 如果删除的是当前会话，切换到最新的会话或创建新会话
    if (this.currentSessionId === sessionId) {
      const sessions = this.getAllSessions();
      this.currentSessionId = sessions.length > 0 ? sessions[0].id : null;
    }

    return true;
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
}

