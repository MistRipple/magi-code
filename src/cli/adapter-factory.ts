/**
 * CLI 适配器工厂
 * 统一管理和创建 CLI 适配器实例
 *
 * 集成 Normalizer 层，输出标准化消息
 */

import { logger, LogCategory } from '../logging';
import { EventEmitter } from 'events';
import { ICLIAdapter, CLIType, AdapterConfig, CLIResponse, CLI_CAPABILITIES, AdapterMessageMeta } from './types';
import { ClaudeAdapter } from './adapters/claude';
import { CodexAdapter } from './adapters/codex';
import { GeminiAdapter } from './adapters/gemini';
import { SessionManager } from './session/session-manager';
import type { CLIQuestion } from './session/print-session';
import { globalEventBus } from '../events';
import {
  createNormalizer,
  BaseNormalizer,
} from '../normalizer';
import {
  StandardMessage,
  StreamUpdate,
  MessageSource,
  MessageType,
  MessageLifecycle,
  InteractionType,
  createInteractionMessage,
  createStandardMessage,
} from '../protocol';

/** 适配器状态信息 */
export interface AdapterStatus {
  type: CLIType;
  connected: boolean;
  busy: boolean;
  state: string;
  installed?: boolean;
}

/** 工厂配置 */
export interface FactoryConfig {
  cwd: string;
  idleTimeout?: number;
  maxTimeout?: number;
  maxOutputChars?: number;
  cliPaths?: Partial<Record<CLIType, string>>;
  env?: Record<string, string>;
}

export type AdapterRole = 'worker' | 'orchestrator';

export interface AdapterOutputScope {
  source?: 'worker' | 'orchestrator' | 'system';
  streamToUI?: boolean;
  adapterRole?: AdapterRole;
  messageMeta?: AdapterMessageMeta;
}

/**
 * CLI 适配器工厂
 * 提供统一的适配器创建、管理和事件转发
 *
 * 集成 Normalizer 层，输出标准化消息
 */
export class CLIAdapterFactory extends EventEmitter {
  private adapters: Map<CLIType, ICLIAdapter> = new Map();
  private orchestratorAdapters: Map<CLIType, ICLIAdapter> = new Map();
  private config: FactoryConfig;
  private sessionManager: SessionManager;
  private outputScopes: Map<string, AdapterOutputScope> = new Map();
  private outputMuteCounts: Map<string, number> = new Map();

  // Normalizer 管理
  private normalizers: Map<string, BaseNormalizer> = new Map();
  private activeMessageIds: Map<string, string> = new Map(); // scopeKey -> messageId
  private traceIdCounter = 0;

  constructor(config: FactoryConfig) {
    super();
    this.config = config;
    this.sessionManager = new SessionManager({
      cwd: config.cwd,
      env: config.env,
      idleTimeoutMs: config.idleTimeout,
      maxOutputChars: config.maxOutputChars,
      commandOverrides: config.cliPaths,
    });
    this.sessionManager.on('sessionEvent', (event) => {
      globalEventBus.emitEvent('cli:session_event', { data: event });
    });

    // 监听 CLI 询问事件（标准消息）
    this.sessionManager.on('question', ({
      cli,
      role,
      question
    }: {
      cli: CLIType;
      role: 'worker' | 'orchestrator';
      question: CLIQuestion;
    }) => {
      const message = createInteractionMessage(
        {
          type: InteractionType.QUESTION,
          requestId: question.questionId,
          prompt: question.content,
          required: true,
          options: [{ value: 'answer', label: '请回答', isDefault: true }],
        },
        role === 'orchestrator' ? 'orchestrator' : 'worker',
        cli,
        this.generateTraceId(),
        {
          metadata: {
            questionId: question.questionId,
            questionPattern: question.pattern,
            questionTimestamp: question.timestamp,
            adapterRole: role,
          }
        }
      );
      this.emit('standardMessage', message);
      this.emit('standardComplete', message);
    });

    // 监听询问超时事件（标准消息）
    this.sessionManager.on('questionTimeout', ({ cli, role, questionId }) => {
      const timeoutMessage = createStandardMessage({
        type: MessageType.ERROR,
        source: role === 'orchestrator' ? 'orchestrator' : 'worker',
        cli,
        traceId: this.generateTraceId(),
        lifecycle: MessageLifecycle.FAILED,
        blocks: [{ type: 'text', content: `CLI 询问超时: ${questionId}` }],
        metadata: { questionId, adapterRole: role },
      });
      this.emit('standardMessage', timeoutMessage);
      this.emit('standardComplete', timeoutMessage);
    });
  }

  // 获取或创建 Normalizer
  private getOrCreateNormalizer(cli: CLIType, source: MessageSource): BaseNormalizer {
    const key = `${cli}:${source}`;
    let normalizer = this.normalizers.get(key);

    if (!normalizer) {
      normalizer = createNormalizer(cli, source, false);

      // 设置 Normalizer 事件监听 - 转发标准消息
      normalizer.on('message', (message: StandardMessage) => {
        this.emit('standardMessage', message);
      });

      normalizer.on('update', (update: StreamUpdate) => {
        this.emit('standardUpdate', update);
      });

      normalizer.on('complete', (messageId: string, message: StandardMessage) => {
        this.emit('standardComplete', message);
        // 清理活跃消息 ID
        for (const [key, id] of this.activeMessageIds) {
          if (id === messageId) {
            this.activeMessageIds.delete(key);
            break;
          }
        }
      });

      this.normalizers.set(key, normalizer);
    }

    return normalizer;
  }

  // 生成追踪 ID
  private generateTraceId(): string {
    return `trace-${Date.now()}-${++this.traceIdCounter}`;
  }

  /**
   * 创建或获取适配器实例
   */
  create(type: CLIType): ICLIAdapter {
    return this.createWithRole(type, 'worker');
  }

  /**
   * 设置适配器事件转发
   * 集成 Normalizer，将原始输出转换为标准消息
   */
  private setupAdapterEvents(adapter: ICLIAdapter, type: CLIType, role: AdapterRole): void {
    const suppressUI = role === 'orchestrator';

    adapter.on('output', (chunk: string) => {
      const scopeKey = this.getScopeKey(type, role);
      if ((this.outputMuteCounts.get(scopeKey) || 0) > 0) {
        return;
      }
      const scope = this.outputScopes.get(scopeKey);
      if (scope?.streamToUI === false) {
        return;
      }

      const source: MessageSource = (scope?.source as MessageSource) || (role === 'orchestrator' ? 'orchestrator' : 'worker');

      // 通过 Normalizer 处理原始输出
      const normalizer = this.getOrCreateNormalizer(type, source);
      let messageId = this.activeMessageIds.get(scopeKey);

      if (!messageId) {
        // 开始新的消息流
        const traceId = this.generateTraceId();
        messageId = normalizer.startStream(traceId, source);
        this.activeMessageIds.set(scopeKey, messageId);
      }

      // 处理输出块
      normalizer.processChunk(messageId, chunk);

      // Normalizer 统一处理消息流，避免重复发送
      // this.emit('output', { type, chunk, source: scope?.source, adapterRole: role });
    });

    adapter.on('response', (response: CLIResponse) => {
      const scopeKey = this.getScopeKey(type, role);
      const scope = this.outputScopes.get(scopeKey);
      const source: MessageSource = (scope?.source as MessageSource) || (role === 'orchestrator' ? 'orchestrator' : 'worker');

      // 结束消息流
      const messageId = this.activeMessageIds.get(scopeKey);
      if (messageId) {
        const normalizer = this.getOrCreateNormalizer(type, source);
        normalizer.endStream(messageId, response.error);
        this.activeMessageIds.delete(scopeKey);
      }

      // Normalizer 统一处理消息流，避免重复发送
      // this.emit('response', { type, response, source: scope?.source, adapterRole: role });
    });

    adapter.on('error', (error: Error) => {
      const scopeKey = this.getScopeKey(type, role);
      const source: MessageSource = role === 'orchestrator' ? 'orchestrator' : 'worker';

      // 错误时结束消息流
      const messageId = this.activeMessageIds.get(scopeKey);
      if (messageId) {
        const normalizer = this.getOrCreateNormalizer(type, source);
        normalizer.endStream(messageId, error.message);
        this.activeMessageIds.delete(scopeKey);
      }

      this.emit('error', { type, error });
    });

    adapter.on('stateChange', (state: string) => {
      if (suppressUI) {
        return;
      }
      this.emit('stateChange', { type, state });
    });
  }

  /**
   * 获取已创建的适配器
   */
  getAdapter(type: CLIType, role: AdapterRole = 'worker'): ICLIAdapter | undefined {
    return this.getAdapterMap(role).get(type);
  }

  /**
   * 检查 CLI 是否可用（已创建且已连接）
   */
  isAvailable(type: CLIType): boolean {
    const adapter = this.adapters.get(type);
    return adapter?.isConnected ?? false;
  }

  /**
   * 获取或创建适配器
   */
  getOrCreate(type: CLIType, role: AdapterRole = 'worker'): ICLIAdapter {
    return this.getAdapterMap(role).get(type) || this.createWithRole(type, role);
  }

  /**
   * 获取所有已创建的适配器
   */
  getAllAdapters(role?: AdapterRole): ICLIAdapter[] {
    if (role) {
      return Array.from(this.getAdapterMap(role).values());
    }
    return [...this.adapters.values(), ...this.orchestratorAdapters.values()];
  }

  /**
   * 获取所有适配器状态
   */
  getAllStatus(): AdapterStatus[] {
    const types: CLIType[] = ['claude', 'codex', 'gemini'];
    return types.map(type => {
      const adapter = this.adapters.get(type);
      return {
        type,
        connected: adapter?.isConnected ?? false,
        busy: adapter?.isBusy ?? false,
        state: adapter?.state ?? 'idle',
      };
    });
  }

  /**
   * 获取所有已连接的适配器
   */
  getConnectedAdapters(): ICLIAdapter[] {
    return this.getAllAdapters().filter(a => a.isConnected);
  }

  /**
   * 获取所有可用（已连接且不忙）的适配器
   */
  getAvailableAdapters(): ICLIAdapter[] {
    return this.getAllAdapters().filter(a => a.isConnected && !a.isBusy);
  }

  /**
   * 连接指定类型的适配器
   */
  async connect(type: CLIType): Promise<ICLIAdapter> {
    const adapter = this.create(type);
    if (!adapter.isConnected) {
      await adapter.connect();
    }
    return adapter;
  }

  /**
   * 连接所有适配器
   */
  async connectAll(): Promise<void> {
    const types: CLIType[] = ['claude', 'codex', 'gemini'];
    await Promise.all(types.map(type => this.connect(type).catch(() => {})));
  }

  /**
   * 检查所有 CLI 的安装状态（轻量检测，不启动进程）
   */
  async checkAllAvailability(): Promise<Record<CLIType, boolean>> {
    const [claude, codex, gemini] = await Promise.all([
      ClaudeAdapter.checkInstalled(),
      CodexAdapter.checkInstalled(),
      GeminiAdapter.checkInstalled(),
    ]);
    return { claude, codex, gemini };
  }

  /**
   * 断开指定类型的适配器
   */
  async disconnect(type: CLIType): Promise<void> {
    const adapter = this.getAdapter(type);
    if (adapter) {
      await adapter.disconnect();
    }
  }

  /**
   * 断开所有适配器
   */
  async disconnectAll(): Promise<void> {
    const promises = this.getAllAdapters().map(a => a.disconnect());
    await Promise.all(promises);
  }

  /**
   * 发送消息到指定 CLI
   * 如果目标 CLI 不支持图片或处于会话恢复模式，会先用 Codex 描述图片
   */
  async sendMessage(
    type: CLIType,
    message: string,
    imagePaths?: string[],
    options?: AdapterOutputScope
  ): Promise<CLIResponse> {
    const role = options?.adapterRole ?? (options?.source === 'orchestrator' ? 'orchestrator' : 'worker');
    const adapter = this.getOrCreate(type, role);
    if (!adapter.isConnected) {
      await adapter.connect();
    }

    const hasImages = imagePaths && imagePaths.length > 0;
    logger.info('CLI.发送消息.请求', { cli: type, hasImages, imageCount: imagePaths?.length || 0, imagePaths }, LogCategory.CLI);

    const scope = options ? { ...options } : null;
    const scopeKey = this.getScopeKey(type, role);
    if (scope) {
      this.outputScopes.set(scopeKey, scope);
    }
    if (options?.streamToUI === false) {
      const count = this.outputMuteCounts.get(scopeKey) || 0;
      this.outputMuteCounts.set(scopeKey, count + 1);
    }


    try {
      return adapter.sendMessage(message, imagePaths, options?.messageMeta);
    } finally {
      if (scope) {
        this.outputScopes.delete(scopeKey);
      }
      if (options?.streamToUI === false) {
        const count = this.outputMuteCounts.get(scopeKey) || 0;
        if (count <= 1) {
          this.outputMuteCounts.delete(scopeKey);
        } else {
          this.outputMuteCounts.set(scopeKey, count - 1);
        }
      }
    }
  }

  /**
   * 中断指定 CLI 的执行
   */
  async interrupt(type: CLIType): Promise<void> {
    const adapter = this.adapters.get(type);
    if (adapter) {
      await adapter.interrupt();
    }
  }

  /**
   * 向 CLI 发送用户输入（用于回答 CLI 询问）
   * @param type CLI 类型
   * @param text 用户输入的文本
   * @param role 适配器角色，默认为 worker
   * @returns 是否发送成功
   */
  writeInput(type: CLIType, text: string, role: 'worker' | 'orchestrator' = 'worker'): boolean {
    return this.sessionManager.writeInput(type, role, text);
  }

  /**
   * 检查 CLI 是否正在等待用户回答
   */
  isWaitingForAnswer(type: CLIType, role: 'worker' | 'orchestrator' = 'worker'): boolean {
    return this.sessionManager.isWaitingForAnswer(type, role);
  }

  /**
   * 向 CLI 面板发送编排者的消息
   * 使用标准消息协议 (StandardMessage)
   *
   * 统一使用 standardMessage API
   */
  private emitOrchestratorMessage(type: CLIType, message: string): void {
    // 提取消息的关键信息,生成简洁的展示内容
    const summary = this.summarizeOrchestratorMessage(message);

    // ✅ 使用 Normalizer 发送标准消息
    const normalizer = this.getOrCreateNormalizer(type, 'orchestrator');

    // 生成唯一的traceId
    const traceId = `orchestrator-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;

    // 发送完整消息 (非流式)
    const messageId = normalizer.startStream(traceId, 'orchestrator');
    normalizer.processChunk(messageId, summary);
    normalizer.endStream(messageId);
  }

  /** 公开方法：向 CLI 面板发送编排者消息 */
  emitOrchestratorMessageToUI(type: CLIType, message: string): void {
    this.emitOrchestratorMessage(type, message);
  }

  /**
   * 将编排者的完整 prompt 转换为简洁的展示摘要
   * 使用 HTML 徽章标签格式，提供专业的视觉效果
   */
  private summarizeOrchestratorMessage(message: string): string {
    // 检测消息类型并生成对应的摘要
    const lines = message.split('\n').filter(l => l.trim());

    // 检测任务分配
    if (message.includes('## 任务') || message.includes('Task:') || message.includes('任务描述')) {
      const taskMatch = message.match(/(?:任务描述|Task|描述)[：:]\s*(.+)/i);
      const filesMatch = message.match(/(?:目标文件|Target files|文件)[：:]\s*(.+)/i);

    let summary = '[TASK]\n';
      if (taskMatch) summary += `${taskMatch[1].trim()}\n`;
      if (filesMatch) summary += `目标文件: ${filesMatch[1].trim()}\n`;

    if (summary === '[TASK]\n') {
      summary += lines.slice(0, 3).join('\n');
    }
    return summary;
  }

    // 检测自检请求
    if (message.includes('自检') || message.includes('self-check') || message.includes('检查是否满足')) {
      return '[SELF CHECK]\n请检查刚才完成的任务是否满足要求...';
    }

    // 检测互检请求
    if (message.includes('互检') || message.includes('peer review') || message.includes('审查')) {
      return '[PEER REVIEW]\n请审查另一个代理完成的任务...';
    }

    // 检测修复请求
    if (message.includes('修复') || message.includes('fix') || message.includes('问题')) {
      return '[FIX]\n请修复之前发现的问题...';
    }

    // 检测分析请求
    if (message.includes('分析') || message.includes('analyze')) {
      return '[ANALYZE]\n请分析任务并生成执行计划...';
    }

    // 检测总结请求
    if (message.includes('总结') || message.includes('summary') || message.includes('汇总')) {
      return '[SUMMARY]\n请汇总执行结果...';
    }

    // 默认：显示消息的前几行
    const preview = lines.slice(0, 5).join('\n');
    const truncated = lines.length > 5 ? '\n...' : '';
    return `[MESSAGE]\n${preview}${truncated}`;
  }

  /**
   * 中断所有 CLI 的执行
   */
  async interruptAll(): Promise<void> {
    const promises = this.getAllAdapters().map(a => a.interrupt());
    await Promise.all(promises);
  }

  /**
   * 重置所有 CLI 的会话
   */
  async resetAllSessions(): Promise<void> {
    await this.sessionManager.stopAll();
  }

  /**
   * 销毁工厂，清理所有资源
   */
  async dispose(): Promise<void> {
    await this.disconnectAll();
    await this.sessionManager.stopAll();
    this.adapters.clear();
    this.orchestratorAdapters.clear();
    this.removeAllListeners();
  }

  private createWithRole(type: CLIType, role: AdapterRole): ICLIAdapter {
    const adapters = this.getAdapterMap(role);
    const existing = adapters.get(type);
    if (existing) {
      return existing;
    }

    const adapterConfig: Omit<AdapterConfig, 'type'> = {
      cwd: this.config.cwd,
      idleTimeout: this.config.idleTimeout,
      maxTimeout: this.config.maxTimeout,
      env: this.config.env,
    };

    let adapter: ICLIAdapter;
    switch (type) {
      case 'claude':
        adapter = new ClaudeAdapter({
          ...adapterConfig,
          role,
          sessionManager: this.sessionManager,
        });
        break;
      case 'codex':
        adapter = new CodexAdapter({
          ...adapterConfig,
          role,
          sessionManager: this.sessionManager,
        });
        break;
      case 'gemini':
        adapter = new GeminiAdapter({
          ...adapterConfig,
          role,
          sessionManager: this.sessionManager,
        });
        break;
      default:
        throw new Error(`Unknown CLI type: ${type}`);
    }

    this.setupAdapterEvents(adapter, type, role);
    adapters.set(type, adapter);
    return adapter;
  }

  private getAdapterMap(role: AdapterRole): Map<CLIType, ICLIAdapter> {
    return role === 'orchestrator' ? this.orchestratorAdapters : this.adapters;
  }

  private getScopeKey(type: CLIType, role: AdapterRole): string {
    return `${role}:${type}`;
  }
}
