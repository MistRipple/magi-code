import { EventEmitter } from 'events';
import { v4 as uuidv4 } from 'uuid';
import * as crypto from 'crypto';
import { PrintSession } from './print-session';
import { InteractiveSession } from './interactive-session';
import type { CLIType } from '../types';
import type { SessionMessage, SessionProcess, SessionProcessOptions, SessionResponse } from './types';
import {
  ORCHESTRATOR_SYSTEM_PROMPT,
  WORKER_SYSTEM_PROMPT_BASE,
} from '../../prompts';
import { ContextManager } from '../../context/context-manager';

/**
 * 会话模式
 */
type SessionMode = 'interactive' | 'oneshot';

/**
 * 角色会话策略
 */
interface SessionStrategy {
  mode: SessionMode;
  persistSession: boolean;
  sessionIdPrefix: string;
}

const SESSION_STRATEGIES: Record<'orchestrator' | 'worker', SessionStrategy> = {
  orchestrator: {
    mode: 'interactive',      // 主编排者使用交互式会话
    persistSession: true,     // 保持会话持久化
    sessionIdPrefix: 'orch',
  },
  worker: {
    mode: 'interactive',      // Worker 也使用交互式会话（利用 --continue 缓存）
    persistSession: true,     // 保持会话持久化以利用缓存
    sessionIdPrefix: 'work',
  },
};

/**
 * 扩展的会话选项
 */
export interface SessionManagerOptions extends SessionProcessOptions {
  /** 上下文管理器（可选） */
  contextManager?: ContextManager;
}

type QueueItem = {
  message: SessionMessage;
  enqueuedAt: number;
  attempts: number;
  resolve: (value: SessionResponse) => void;
  reject: (error: Error) => void;
};

interface ManagedSession {
  process: SessionProcess;
  busy: boolean;
  queue: QueueItem[];
  processing: boolean;
  failures: number;
  lastOutputAt: number;
  lastStartAt?: number;
  healthTimer?: NodeJS.Timeout;
  suppressOutput: boolean;
  lastSnapshot?: string;
}

export class SessionManager extends EventEmitter {
  private sessions: Map<string, ManagedSession> = new Map();
  private readonly softInterruptMs: number;
  private readonly hardTimeoutMs: number;
  private readonly contextManager?: ContextManager;

  constructor(private readonly options: SessionManagerOptions) {
    super();
    const idleMs = options.idleTimeoutMs ?? 120000;
    this.softInterruptMs = Math.max(15000, Math.floor(idleMs * 0.7));
    this.hardTimeoutMs = Math.max(30000, idleMs);
    this.contextManager = options.contextManager;
  }

  private getKey(cli: CLIType, role: 'worker' | 'orchestrator'): string {
    return `${role}:${cli}`;
  }

  private resolveClaudePermissionMode(role: 'worker' | 'orchestrator'): string {
    if (role === 'worker') {
      return 'acceptEdits';
    }
    const allowed = new Set([
      'acceptEdits',
      'bypassPermissions',
      'default',
      'delegate',
      'dontAsk',
      'plan',
    ]);
    const envValue = (process.env.MULTICLI_CLAUDE_ORCH_PERMISSION || '').trim();
    if (envValue && allowed.has(envValue)) {
      return envValue;
    }
    return 'plan';
  }

  private buildArgs(cli: CLIType, role: 'worker' | 'orchestrator'): string[] {
    const args: string[] = [];
    const isOrchestrator = role === 'orchestrator';

    // 获取角色对应的 System Prompt（根据 role 和 cli 类型）
    const systemPrompt = this.getSystemPromptForRole(role, cli);

    if (cli === 'claude') {
      args.push('-p', '--output-format', 'stream-json', '--verbose');
      const permissionMode = this.resolveClaudePermissionMode(role);
      if (isOrchestrator) {
        // 编排者仅分析，不允许直接修改文件
        args.push('--permission-mode', permissionMode);
      } else {
        args.push('--permission-mode', permissionMode);
      }
      args.push('--continue');

      // 注入 System Prompt（追加到默认 prompt 后）
      if (systemPrompt) {
        args.push('--append-system-prompt', systemPrompt);
      }
    }
    if (cli === 'codex') {
      args.push('exec', '--skip-git-repo-check');
      if (isOrchestrator) {
        args.push('--approval', 'suggest');
      } else {
        args.push('--full-auto');
      }
      // 注意：Codex 不支持 --instructions 参数，系统指令通过消息前缀注入
    }
    if (cli === 'gemini') {
      args.push('--output-format', 'stream-json');
      if (isOrchestrator) {
        args.push('--approval-mode', 'prompt');
      } else {
        args.push('--approval-mode', 'yolo');
      }
      // Gemini CLI 暂不支持 system prompt 参数，通过消息前缀注入
    }

    return args;
  }

  /**
   * 获取角色对应的 System Prompt
   *
   * 角色架构：
   * - Orchestrator Claude: 主编排者，专职分析规划，禁止执行编码
   * - Worker Claude: 子代理，复杂架构、多文件重构
   * - Worker Codex: 子代理，后端开发、Bug修复
   * - Worker Gemini: 子代理，前端开发、UI/UX
   */
  private getSystemPromptForRole(role: 'worker' | 'orchestrator', cli?: CLIType): string {
    if (role === 'orchestrator') {
      // 主编排者专用 Prompt（仅 Claude 可作为编排者）
      return ORCHESTRATOR_SYSTEM_PROMPT;
    }

    // 子代理根据 CLI 类型使用专业化 Prompt
    const workerType = cli as 'claude' | 'codex' | 'gemini' | undefined;
    if (workerType) {
      const { buildWorkerSystemPrompt } = require('../../prompts/worker-system');
      return buildWorkerSystemPrompt(workerType, {
        workspace: this.options.cwd,
      });
    }

    // 回退到基础 Worker Prompt
    return WORKER_SYSTEM_PROMPT_BASE;
  }

  /**
   * 生成稳定的会话 ID（基于工作区路径）
   */
  private generateSessionId(role: 'worker' | 'orchestrator', cli: CLIType): string {
    const strategy = SESSION_STRATEGIES[role];
    const hash = crypto.createHash('md5').update(this.options.cwd).digest('hex').substring(0, 8);
    return `${strategy.sessionIdPrefix}-${cli}-${hash}`;
  }

  async startSession(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void> {
    const key = this.getKey(cli, role);
    if (this.sessions.has(key)) return;

    const strategy = SESSION_STRATEGIES[role];
    const args = this.buildArgs(cli, role);
    const command = this.options.commandOverrides?.[cli] ?? cli;

    let sessionProcess: SessionProcess;

    // 根据角色策略选择会话模式
    if (strategy.mode === 'interactive' && cli === 'claude') {
      // 主编排者使用交互式长进程（仅 Claude 支持）
      const sessionId = this.generateSessionId(role, cli);
      sessionProcess = new InteractiveSession({
        cli,
        cwd: this.options.cwd,
        env: this.options.env,
        command,
        args,
        idleTimeoutMs: this.options.idleTimeoutMs,
        sessionId,
      });
      this.emit('log', `[SessionManager] 使用交互式会话模式: ${cli}/${role} (sessionId: ${sessionId})`);
    } else {
      // 子代理使用单次进程模式
      sessionProcess = new PrintSession({
        cli,
        cwd: this.options.cwd,
        env: this.options.env,
        idleTimeoutMs: this.options.idleTimeoutMs,
        command,
        args,
      });
      this.emit('log', `[SessionManager] 使用单次进程模式: ${cli}/${role}`);
    }

    // 设置事件监听
    this.setupSessionEventHandlers(sessionProcess, cli, role, key);

    await sessionProcess.start();

    const entry: ManagedSession = {
      process: sessionProcess,
      busy: false,
      queue: [],
      processing: false,
      failures: 0,
      lastOutputAt: Date.now(),
      suppressOutput: false,
      lastSnapshot: undefined,
    };
    entry.healthTimer = this.startHealthMonitor(cli, role);
    this.sessions.set(key, entry);
    this.emit('sessionEvent', { type: 'start', cli, role, mode: strategy.mode });
  }

  /**
   * 设置会话事件处理器
   */
  private setupSessionEventHandlers(
    sessionProcess: SessionProcess,
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    key: string
  ): void {
    sessionProcess.on('output', (...args: unknown[]) => {
      const chunk = args[0] as string;
      const entry = this.sessions.get(key);
      if (entry) {
        entry.lastOutputAt = Date.now();
        if (entry.suppressOutput) {
          return;
        }
      }
      this.emit('output', { cli, role, chunk });
    });

    // 监听 CLI 询问事件
    sessionProcess.on('question', (...args: unknown[]) => {
      const question = args[0];
      this.emit('question', { cli, role, question });
    });

    // 监听询问超时事件
    sessionProcess.on('questionTimeout', (...args: unknown[]) => {
      const data = args[0] as Record<string, unknown>;
      this.emit('questionTimeout', { cli, role, ...data });
    });

    // 监听日志事件
    sessionProcess.on('log', (...args: unknown[]) => {
      const message = args[0] as string;
      this.emit('log', message);
    });

    // 监听进程退出/关闭事件
    sessionProcess.on('exit', () => {
      this.handleSessionExit(cli, role, key);
    });

    sessionProcess.on('close', () => {
      this.handleSessionExit(cli, role, key);
    });
  }

  /**
   * 处理会话退出
   */
  private handleSessionExit(cli: CLIType, role: 'worker' | 'orchestrator', key: string): void {
    const entry = this.sessions.get(key);
    if (entry) {
      entry.failures += 1;
      this.emit('sessionEvent', { type: 'exit', cli, role });

      // 如果是交互式会话（主编排者），尝试自动恢复
      const strategy = SESSION_STRATEGIES[role];
      if (strategy.mode === 'interactive') {
        this.emit('log', `[SessionManager] 交互式会话意外退出，将在下次调用时自动恢复: ${cli}/${role}`);
        this.sessions.delete(key);
      }
    }
  }

  async stopSession(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) return;
    if (entry.healthTimer) {
      clearInterval(entry.healthTimer);
    }
    entry.queue = [];
    entry.suppressOutput = false;
    await entry.process.stop();
    this.sessions.delete(key);
  }

  async send(cli: CLIType, role: 'worker' | 'orchestrator', message: SessionMessage): Promise<SessionResponse> {
    const key = this.getKey(cli, role);
    let entry = this.sessions.get(key);
    if (!entry) {
      await this.startSession(cli, role);
      entry = this.sessions.get(key);
    }
    if (!entry) {
      throw new Error(`Session not available: ${cli}`);
    }

    // 为需要消息前缀注入的 CLI 添加 System Prompt 前缀
    const processedMessage = this.injectMessagePrefix(cli, role, message);

    return new Promise<SessionResponse>((resolve, reject) => {
      entry!.queue.push({
        message: processedMessage,
        enqueuedAt: Date.now(),
        attempts: 0,
        resolve,
        reject,
      });
      this.emit('sessionEvent', { type: 'enqueue', cli, role, requestId: message.requestId });
      void this.processQueue(cli, role);
    });
  }

  /**
   * 为消息注入上下文前缀
   *
   * 注入策略：
   * - Orchestrator (交互式): 无需注入，CLI 自动保持历史上下文
   * - Worker (单次进程): 注入精简上下文 (~2000 tokens)
   * - Gemini: 额外需要通过消息前缀注入 System Prompt
   */
  private injectMessagePrefix(cli: CLIType, role: 'worker' | 'orchestrator', message: SessionMessage): SessionMessage {
    const strategy = SESSION_STRATEGIES[role];
    let content = message.content;

    // 交互式会话（主编排者）无需注入上下文，CLI 自动保持历史
    if (strategy.mode === 'interactive') {
      return message;
    }

    // 构建注入内容的各部分
    const parts: string[] = [];

    // 1. Gemini 需要注入 System Prompt（因为不支持命令行参数）
    if (cli === 'gemini') {
      const systemPrompt = this.getSystemPromptForRole(role, cli);
      parts.push(systemPrompt);
    }

    // 2. Worker 需要注入精简上下文
    const contextPrefix = this.buildWorkerContextPrefix(cli, message);
    if (contextPrefix) {
      parts.push(contextPrefix);
    }

    // 3. 原始用户消息
    parts.push(content);

    // 用分隔符连接各部分
    content = parts.join('\n\n---\n\n');

    return {
      ...message,
      content,
    };
  }

  /**
   * 构建 Worker 上下文前缀
   * 包含：当前任务信息、关键决策、近期代码变更等
   */
  private buildWorkerContextPrefix(cli: CLIType, message: SessionMessage): string {
    const sections: string[] = [];

    // 1. 任务信息（如果有）
    if (message.taskId) {
      sections.push(`## 当前任务`);
      sections.push(`- 任务 ID: ${message.taskId}`);
      if (message.subTaskId) {
        sections.push(`- 子任务 ID: ${message.subTaskId}`);
      }
    }

    // 2. 从 ContextManager 获取结构化上下文
    if (this.contextManager) {
      const contextSlice = this.contextManager.getContextSlice({
        maxTokens: 2000,  // Worker 精简上下文
        includeMemory: true,
        includeRecent: false,  // 不包含最近对话（Worker 是独立任务）
        memorySummary: {
          includeCurrentTasks: true,
          includeKeyDecisions: 3,
          includeCodeChanges: 5,
          includePendingIssues: true,
        },
      });

      // contextSlice 已包含格式化的上下文，直接添加
      if (contextSlice && contextSlice.trim()) {
        sections.push(contextSlice);
      }
    }

    return sections.length > 0 ? sections.join('\n') : '';
  }

  async interrupt(cli: CLIType, role: 'worker' | 'orchestrator', reason?: string): Promise<void> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) return;
    await entry.process.interrupt(reason);
  }

  /**
   * 向 CLI 发送用户输入（用于回答 CLI 询问）
   */
  writeInput(cli: CLIType, role: 'worker' | 'orchestrator', text: string): boolean {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) {
      console.log(`[SessionManager] writeInput failed: session not found for ${cli}/${role}`);
      return false;
    }
    return entry.process.writeInput(text);
  }

  /**
   * 检查 CLI 是否正在等待用户回答
   */
  isWaitingForAnswer(cli: CLIType, role: 'worker' | 'orchestrator'): boolean {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) return false;
    return entry.process.isWaitingForAnswer;
  }

  /**
   * 检查会话是否存活
   */
  isSessionAlive(cli: CLIType, role: 'worker' | 'orchestrator'): boolean {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry) return false;
    return entry.process.isAlive;
  }

  /**
   * 获取会话模式
   */
  getSessionMode(role: 'worker' | 'orchestrator'): string {
    return SESSION_STRATEGIES[role].mode;
  }

  async stopAll(): Promise<void> {
    const entries = Array.from(this.sessions.values());
    entries.forEach(entry => {
      if (entry.healthTimer) {
        clearInterval(entry.healthTimer);
      }
      entry.queue = [];
      entry.suppressOutput = false;
    });
    await Promise.all(entries.map(entry => entry.process.stop()));
    this.sessions.clear();
  }

  private async processQueue(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (!entry || entry.processing) return;
    entry.processing = true;
    try {
      while (entry.queue.length > 0) {
        const item = entry.queue.shift();
        if (!item) break;
        try {
          const response = await this.executeItem(cli, role, entry, item);
          item.resolve(response);
        } catch (error) {
          const err = error instanceof Error ? error : new Error(String(error));
          item.reject(err);
        }
      }
    } finally {
      entry.processing = false;
    }
  }

  private async executeItem(
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    entry: ManagedSession,
    item: QueueItem
  ): Promise<SessionResponse> {
    if (!entry.process.isAlive) {
      entry = await this.restartSession(cli, role, 'process_dead');
    }

    entry.busy = true;
    entry.suppressOutput = Boolean(item.message.silent);
    entry.lastStartAt = Date.now();
    this.emit('sessionEvent', { type: 'start', cli, role, requestId: item.message.requestId });

    const snapshot = this.extractSnapshot(item.message.metadata);
    if (snapshot) {
      entry.lastSnapshot = snapshot;
    }

    let softTimer: NodeJS.Timeout | undefined;
    softTimer = setTimeout(() => {
      void entry.process.interrupt('soft_timeout');
      this.emit('sessionEvent', { type: 'soft_interrupt', cli, role, requestId: item.message.requestId });
    }, this.softInterruptMs);

    let hardTimer: NodeJS.Timeout | undefined;
    const hardTimeoutPromise = new Promise<SessionResponse>((_, reject) => {
      hardTimer = setTimeout(() => {
        void this.restartSession(cli, role, 'hard_timeout');
        reject(new Error(`${cli} session hard timeout`));
      }, this.hardTimeoutMs);
    });

    try {
      const response = await Promise.race([entry.process.send(item.message), hardTimeoutPromise]);
      if (this.isEmptyResponse(response)) {
        const err = new Error(`${cli} session empty response`);
        if (this.isRecoverable(err) && item.attempts < 1) {
          item.attempts += 1;
          entry = await this.restartSession(cli, role, 'empty_response');
          this.emit('sessionEvent', { type: 'retry', cli, role, requestId: item.message.requestId, reason: 'empty_response' });
          return await this.executeItem(cli, role, entry, item);
        }
        this.emit('sessionEvent', { type: 'failed', cli, role, requestId: item.message.requestId, reason: 'empty_response' });
        throw err;
      }
      entry.failures = 0;
      this.emit('sessionEvent', { type: 'complete', cli, role, requestId: item.message.requestId });
      return response;
    } catch (error) {
      const err = error instanceof Error ? error : new Error(String(error));
      if (this.isRecoverable(err) && item.attempts < 1) {
        item.attempts += 1;
        entry = await this.restartSession(cli, role, 'recoverable_error');
        this.emit('sessionEvent', { type: 'retry', cli, role, requestId: item.message.requestId });
        return await this.executeItem(cli, role, entry, item);
      }
      entry.failures += 1;
      this.emit('sessionEvent', { type: 'failed', cli, role, requestId: item.message.requestId });
      throw err;
    } finally {
      if (softTimer) clearTimeout(softTimer);
      if (hardTimer) clearTimeout(hardTimer);
      entry.busy = false;
      entry.suppressOutput = false;
    }
  }

  private isRecoverable(error: Error): boolean {
    const message = error.message.toLowerCase();
    return (
      message.includes('timeout') ||
      message.includes('protocol') ||
      message.includes('output overflow') ||
      message.includes('session exited') ||
      message.includes('empty response')
    );
  }

  private isEmptyResponse(response: SessionResponse): boolean {
    if (response.error) return false;
    const content = (response.content || '').trim();
    if (content) return false;
    const raw = (response.raw || '').trim();
    return !raw;
  }

  private async restartSession(
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    reason: string
  ): Promise<ManagedSession> {
    const key = this.getKey(cli, role);
    const entry = this.sessions.get(key);
    if (entry) {
      await entry.process.stop();
      this.sessions.delete(key);
      this.emit('sessionEvent', { type: 'restart', cli, role, reason });
    }
    await this.startSession(cli, role);
    const next = this.sessions.get(key);
    if (!next) {
      throw new Error(`${cli} session restart failed`);
    }
    if (entry?.lastSnapshot) {
      await this.rehydrateSession(cli, role, next, entry.lastSnapshot);
    }
    return next;
  }

  private extractSnapshot(metadata?: Record<string, unknown>): string | null {
    const value = metadata?.['contextSnapshot'];
    if (!value || typeof value !== 'string') return null;
    return value.length > 6000 ? value.slice(0, 6000) : value;
  }

  private async rehydrateSession(
    cli: CLIType,
    role: 'worker' | 'orchestrator',
    entry: ManagedSession,
    snapshot: string
  ): Promise<void> {
    const requestId = uuidv4();
    const content = [
      '会话恢复：以下是最近上下文摘要，仅用于记忆，不要执行任务或修改文件。',
      '',
      snapshot.trim(),
      '',
      '请回复“OK”。'
    ].join('\n');
    const message: SessionMessage = {
      requestId,
      cli,
      role,
      content,
      metadata: { intent: 'rehydrate' },
      silent: true,
    };
    entry.suppressOutput = true;
    try {
      await entry.process.send(message);
      this.emit('sessionEvent', { type: 'rehydrate', cli, role, requestId });
    } catch (error) {
      const err = error instanceof Error ? error.message : String(error);
      this.emit('sessionEvent', { type: 'rehydrate_failed', cli, role, requestId, error: err });
    } finally {
      entry.suppressOutput = false;
    }
  }

  private startHealthMonitor(cli: CLIType, role: 'worker' | 'orchestrator'): NodeJS.Timeout {
    const intervalMs = this.options.heartbeatMs ?? 15000;
    return setInterval(() => {
      const key = this.getKey(cli, role);
      const entry = this.sessions.get(key);
      if (!entry) return;
      if (!entry.process.isAlive) {
        void this.restartSession(cli, role, 'not_alive');
        return;
      }
      if (entry.busy && entry.lastStartAt) {
        const elapsed = Date.now() - entry.lastStartAt;
        if (elapsed > this.hardTimeoutMs + 5000) {
          void this.restartSession(cli, role, 'stuck');
        }
      }
    }, intervalMs);
  }
}
