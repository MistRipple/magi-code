/**
 * LLM 适配器抽象基类
 *
 * 🔧 统一消息通道（unified-message-channel-design.md v2.5）
 *
 * 消息流架构（3层）：
 * Layer 1: Normalizer.emit('message')
 * Layer 2: Adapter.setupNormalizerEvents() → messageHub.sendMessage() [直接调用]
 * Layer 3: MessageHub → emit('standard:message') → WebviewProvider.postMessage()
 */

import { EventEmitter } from 'events';
import { AgentType, AgentRole, LLMConfig, TokenUsage } from '../../types/agent-types';
import { LLMClient } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { logger, LogCategory } from '../../logging';
import { MESSAGE_EVENTS, ADAPTER_EVENTS } from '../../protocol/event-names';

/**
 * 适配器状态
 */
export enum AdapterState {
  DISCONNECTED = 'disconnected',
  CONNECTING = 'connecting',
  CONNECTED = 'connected',
  BUSY = 'busy',
  ERROR = 'error',
}

/**
 * 适配器事件
 */
export interface AdapterEvents {
  stateChange: (state: AdapterState) => void;
  error: (error: Error) => void;
  message: (content: string) => void;
  toolCall: (toolName: string, args: any) => void;
  toolResult: (toolName: string, result: string) => void;
}

/**
 * LLM 适配器基类
 *
 * 🔧 统一消息通道：持有 MessageHub 引用，消息通过 MessageHub 发送到前端。
 * 不再通过 UnifiedMessageBus 事件转发链，减少层级，提高效率。
 */
export abstract class BaseLLMAdapter extends EventEmitter {
  protected state: AdapterState = AdapterState.DISCONNECTED;
  protected client: LLMClient;
  protected normalizer: BaseNormalizer;
  protected toolManager: ToolManager;
  protected config: LLMConfig;
  protected currentTraceId?: string;
  protected lastTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };
  protected totalTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };

  /**
   * 消息出口 - 直接发送消息到前端
   * 🔧 统一消息通道：替代 UnifiedMessageBus，成为唯一出口
   */
  protected messageHub: MessageHub;

  /**
   * 控制是否将消息流式传输到 UI
   * - true: 消息会发送到前端（默认）
   * - false: 静默模式，用于内部后台操作（如内存压缩）
   */
  protected _streamToUI: boolean = true;

  constructor(
    client: LLMClient,
    normalizer: BaseNormalizer,
    toolManager: ToolManager,
    config: LLMConfig,
    messageHub: MessageHub
  ) {
    super();
    this.client = client;
    this.normalizer = normalizer;
    this.toolManager = toolManager;
    this.config = config;
    this.messageHub = messageHub;

    // 设置 Normalizer 事件处理，直接发送到 MessageHub
    this.setupNormalizerEvents();
  }

  /**
   * 设置是否流式传输到 UI
   */
  setStreamToUI(enabled: boolean): void {
    this._streamToUI = enabled;
  }

  /**
   * 获取当前 streamToUI 状态
   */
  get streamToUI(): boolean {
    return this._streamToUI;
  }

  /**
   * 使用当前请求上下文启动流式消息
   * 优先复用占位消息 ID，确保 UI 端流式更新命中同一条消息
   */
  protected startStreamWithContext(): string {
    if (!this.currentTraceId) {
      this.currentTraceId = this.messageHub.getTraceId();
    }
    const requestId = this.messageHub.getRequestContext();
    const boundMessageId = requestId ? this.messageHub.getRequestMessageId(requestId) : undefined;

    // 🔧 调试日志：追踪 ID 复用机制
    console.log('[BaseAdapter] startStreamWithContext:', {
      requestId,
      boundMessageId,
      hasRequestId: !!requestId,
      hasBoundMessageId: !!boundMessageId,
    });

    return this.normalizer.startStream(this.currentTraceId, undefined, boundMessageId);
  }

  /**
   * 设置 Normalizer 事件处理
   *
   * 🔧 统一消息通道：消息直接发送到 MessageHub（Layer 2 → Layer 3）：
   * - 根据 streamToUI 控制是否发送
   * - 跳过 AdapterFactory 和 WebviewProvider 的中间转发层
   * - 错误事件仍通过 EventEmitter 传递（需要特殊处理）
   */
  private setupNormalizerEvents(): void {
    // 消息开始/流式：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.MESSAGE, (message) => {
      console.log('[BaseAdapter] Normalizer MESSAGE event:', {
        messageId: message?.id,
        category: message?.category,
        lifecycle: message?.lifecycle,
        streamToUI: this._streamToUI,
        hasMessageHub: !!this.messageHub,
      });
      if (this._streamToUI) {
        this.messageHub.sendMessage(message);
      } else {
        console.log('[BaseAdapter] 消息被阻止 (streamToUI=false):', {
          messageId: message?.id,
          category: message?.category,
        });
      }
    });

    // 消息完成：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.COMPLETE, (_messageId, message) => {
      console.log('[BaseAdapter] Normalizer COMPLETE event:', {
        messageId: message?.id,
        category: message?.category,
        lifecycle: message?.lifecycle,
        blocksCount: message?.blocks?.length,
        streamToUI: this._streamToUI,
      });
      if (this._streamToUI) {
        this.messageHub.sendMessage(message);
      } else {
        console.log('[BaseAdapter] 完成消息被阻止 (streamToUI=false):', {
          messageId: message?.id,
          category: message?.category,
          blocksCount: message?.blocks?.length,
        });
      }
    });

    // 流式更新：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.UPDATE, (update) => {
      if (this._streamToUI) {
        this.messageHub.sendUpdate(update);
      }
    });

    // 错误事件：通过 EventEmitter 传递（需要特殊处理）
    this.normalizer.on(MESSAGE_EVENTS.ERROR, (error) => {
      this.emit(ADAPTER_EVENTS.NORMALIZER_ERROR, error);
    });
  }

  /**
   * 获取代理类型
   */
  abstract get agent(): AgentType;

  /**
   * 获取代理角色
   */
  abstract get role(): AgentRole;

  /**
   * 连接到 LLM
   *
   * 直接标记为已连接状态，不再发送测试请求。
   * 如果配置有误（API key 错误等），sendMessage 时会抛出错误并返回给用户。
   * 这样避免了第一条消息发送两次 LLM 请求的性能问题。
   */
  async connect(): Promise<void> {
    if (this.state === AdapterState.CONNECTED) {
      return;
    }

    // 直接标记为已连接，跳过 testConnection 调用
    // 原因：testConnection 会发送一个 "test" 消息到 LLM API，
    // 这导致第一条用户消息需要等待两次 LLM 往返，延迟翻倍
    this.setState(AdapterState.CONNECTED);
    logger.info(`${this.agent} adapter connected`, undefined, LogCategory.LLM);
  }

  /**
   * 断开连接
   */
  async disconnect(): Promise<void> {
    if (this.state === AdapterState.DISCONNECTED) {
      return;
    }

    this.setState(AdapterState.DISCONNECTED);
    logger.info(`${this.agent} adapter disconnected`, undefined, LogCategory.LLM);
  }

  /**
   * 发送消息
   */
  abstract sendMessage(message: string, images?: string[]): Promise<string>;

  /**
   * 中断当前请求
   */
  abstract interrupt(): Promise<void>;

  /**
   * 获取连接状态
   */
  get isConnected(): boolean {
    return this.state === AdapterState.CONNECTED || this.state === AdapterState.BUSY;
  }

  /**
   * 获取忙碌状态
   */
  get isBusy(): boolean {
    return this.state === AdapterState.BUSY;
  }

  /**
   * 获取最近一次请求的 Token 使用
   */
  getLastTokenUsage(): TokenUsage {
    return { ...this.lastTokenUsage };
  }

  /**
   * 获取累计 Token 使用
   */
  getTotalTokenUsage(): TokenUsage {
    return { ...this.totalTokenUsage };
  }

  /**
   * 记录 Token 使用
   */
  protected recordTokenUsage(usage?: Partial<TokenUsage>): void {
    if (!usage) return;
    const inputTokens = usage.inputTokens || 0;
    const outputTokens = usage.outputTokens || 0;
    const cacheReadTokens = usage.cacheReadTokens || 0;
    const cacheWriteTokens = usage.cacheWriteTokens || 0;

    this.lastTokenUsage = {
      inputTokens,
      outputTokens,
      cacheReadTokens: cacheReadTokens || undefined,
      cacheWriteTokens: cacheWriteTokens || undefined,
    };

    this.totalTokenUsage.inputTokens += inputTokens;
    this.totalTokenUsage.outputTokens += outputTokens;
    if (cacheReadTokens) {
      this.totalTokenUsage.cacheReadTokens =
        (this.totalTokenUsage.cacheReadTokens || 0) + cacheReadTokens;
    }
    if (cacheWriteTokens) {
      this.totalTokenUsage.cacheWriteTokens =
        (this.totalTokenUsage.cacheWriteTokens || 0) + cacheWriteTokens;
    }
  }

  /**
   * 获取当前状态
   */
  getState(): AdapterState {
    return this.state;
  }

  /**
   * 设置状态
   */
  protected setState(state: AdapterState): void {
    if (this.state !== state) {
      this.state = state;
      this.emit('stateChange', state);
    }
  }

  /**
   * 发出错误事件
   */
  protected emitError(error: Error): void {
    this.emit('error', error);
    logger.error(`${this.agent} adapter error`, { error: error.message }, LogCategory.LLM);
  }

  /**
   * 生成 trace ID
   */
  protected generateTraceId(): string {
    return `trace-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }
}
