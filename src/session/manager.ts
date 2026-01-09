/**
 * 会话管理器
 * 协调 CLI 适配器和会话存储
 */

import * as vscode from 'vscode';
import { EventEmitter } from 'events';
import { v4 as uuidv4 } from 'uuid';
import { CLIAdapterFactory } from '../cli/adapter-factory';
import { SessionStorage } from './storage';
import {
  Session,
  SessionMessage,
  SessionMeta,
  CLIType,
  CLIResponse,
  ICLIAdapter,
} from '../cli/types';

/** 会话管理器事件 */
export interface SessionManagerEvents {
  sessionCreated: (session: Session) => void;
  sessionSwitched: (session: Session) => void;
  sessionUpdated: (session: Session) => void;
  sessionDeleted: (id: string) => void;
  messageAdded: (message: SessionMessage) => void;
  cliOutput: (cli: CLIType, chunk: string) => void;
  cliResponse: (cli: CLIType, response: CLIResponse) => void;
  error: (error: Error) => void;
}

/**
 * 会话管理器
 */
export class SessionManager extends EventEmitter {
  private factory: CLIAdapterFactory;
  private storage: SessionStorage;
  private currentSession: Session | null = null;
  private sessions: Map<string, Session> = new Map();

  constructor(context: vscode.ExtensionContext, cwd: string) {
    super();
    this.factory = new CLIAdapterFactory({ cwd });
    this.storage = new SessionStorage(context);
  }

  /** 获取当前会话 */
  get current(): Session | null {
    return this.currentSession;
  }

  /** 获取当前会话 ID */
  get currentId(): string | null {
    return this.currentSession?.id || null;
  }

  /** 初始化，加载会话列表 */
  async initialize(): Promise<void> {
    const metas = await this.storage.list();
    // 预加载最近的会话
    if (metas.length > 0) {
      const recent = await this.storage.load(metas[0].id);
      if (recent) {
        this.sessions.set(recent.id, recent);
        this.currentSession = recent;
      }
    }
  }

  /** 创建新会话 */
  async createSession(name?: string): Promise<Session> {
    const session: Session = {
      id: uuidv4(),
      name,
      messages: [],
      createdAt: Date.now(),
      updatedAt: Date.now(),
    };

    this.sessions.set(session.id, session);
    await this.storage.save(session);
    this.currentSession = session;

    this.emit('sessionCreated', session);
    return session;
  }

  /** 切换会话 */
  async switchSession(id: string): Promise<Session | null> {
    let session = this.sessions.get(id);
    if (!session) {
      session = await this.storage.load(id);
      if (session) {
        this.sessions.set(id, session);
      }
    }

    if (session) {
      this.currentSession = session;
      this.emit('sessionSwitched', session);
    }

    return session || null;
  }

  /** 重命名会话 */
  async renameSession(id: string, name: string): Promise<void> {
    const session = this.sessions.get(id) || await this.storage.load(id);
    if (session) {
      session.name = name;
      session.updatedAt = Date.now();
      this.sessions.set(id, session);
      await this.storage.save(session);
      this.emit('sessionUpdated', session);
    }
  }

  /** 删除会话 */
  async deleteSession(id: string): Promise<void> {
    this.sessions.delete(id);
    await this.storage.delete(id);

    if (this.currentSession?.id === id) {
      this.currentSession = null;
      // 切换到最近的会话
      const metas = await this.storage.list();
      if (metas.length > 0) {
        await this.switchSession(metas[0].id);
      }
    }

    this.emit('sessionDeleted', id);
  }

  /** 获取会话列表 */
  async listSessions(): Promise<SessionMeta[]> {
    return this.storage.list();
  }

  /** 发送消息到 CLI */
  async sendMessage(content: string, cli?: CLIType): Promise<CLIResponse> {
    // 确保有当前会话
    if (!this.currentSession) {
      await this.createSession();
    }

    // 添加用户消息
    const userMessage: SessionMessage = {
      id: uuidv4(),
      role: 'user',
      content,
      cli,
      timestamp: Date.now(),
    };
    this.addMessage(userMessage);

    // 获取或创建适配器
    const targetCli = cli || 'claude'; // 默认使用 Claude
    const adapter = await this.factory.connect(targetCli);

    // 设置输出监听
    const outputHandler = (chunk: string) => {
      this.emit('cliOutput', targetCli, chunk);
    };
    adapter.on('output', outputHandler);

    try {
      // 发送消息
      const response = await adapter.sendMessage(content);

      // 添加助手消息
      const assistantMessage: SessionMessage = {
        id: uuidv4(),
        role: 'assistant',
        content: response.content,
        cli: targetCli,
        timestamp: Date.now(),
        fileChanges: response.fileChanges,
      };
      this.addMessage(assistantMessage);

      this.emit('cliResponse', targetCli, response);
      return response;
    } finally {
      adapter.off('output', outputHandler);
    }
  }

  /** 添加消息到当前会话 */
  private addMessage(message: SessionMessage): void {
    if (!this.currentSession) return;

    this.currentSession.messages.push(message);
    this.currentSession.updatedAt = Date.now();

    // 异步保存
    this.storage.save(this.currentSession).catch(err => {
      this.emit('error', err);
    });

    this.emit('messageAdded', message);
  }

  /** 中断当前 CLI 操作 */
  async interrupt(cli?: CLIType): Promise<void> {
    if (cli) {
      const adapter = this.factory.getAdapter(cli);
      if (adapter) {
        await adapter.interrupt();
      }
    } else {
      // 中断所有
      const adapters = this.factory.getConnectedAdapters();
      await Promise.all(adapters.map(a => a.interrupt()));
    }
  }

  /** 获取 CLI 适配器 */
  getAdapter(cli: CLIType): ICLIAdapter | undefined {
    return this.factory.getAdapter(cli);
  }

  /** 获取适配器工厂 */
  getFactory(): CLIAdapterFactory {
    return this.factory;
  }

  /** 销毁管理器 */
  async dispose(): Promise<void> {
    await this.factory.dispose();
  }

  /** 事件监听类型 */
  on<K extends keyof SessionManagerEvents>(
    event: K,
    listener: SessionManagerEvents[K]
  ): this {
    return super.on(event, listener);
  }

  off<K extends keyof SessionManagerEvents>(
    event: K,
    listener: SessionManagerEvents[K]
  ): this {
    return super.off(event, listener);
  }

  emit<K extends keyof SessionManagerEvents>(
    event: K,
    ...args: Parameters<SessionManagerEvents[K]>
  ): boolean {
    return super.emit(event, ...args);
  }
}

