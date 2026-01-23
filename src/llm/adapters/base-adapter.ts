/**
 * LLM 适配器抽象基类
 * 替代 LLM 适配器，使用 LLM API 直接通信
 */

import { EventEmitter } from 'events';
import { AgentType, AgentRole, LLMConfig } from '../../types/agent-types';
import { LLMClient } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { logger, LogCategory } from '../../logging';

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
 */
export abstract class BaseLLMAdapter extends EventEmitter {
  protected state: AdapterState = AdapterState.DISCONNECTED;
  protected client: LLMClient;
  protected normalizer: BaseNormalizer;
  protected toolManager: ToolManager;
  protected config: LLMConfig;
  protected currentTraceId?: string;

  constructor(
    client: LLMClient,
    normalizer: BaseNormalizer,
    toolManager: ToolManager,
    config: LLMConfig
  ) {
    super();
    this.client = client;
    this.normalizer = normalizer;
    this.toolManager = toolManager;
    this.config = config;

    // 转发 normalizer 事件
    this.setupNormalizerEvents();
  }

  /**
   * 设置 normalizer 事件转发
   */
  private setupNormalizerEvents(): void {
    this.normalizer.on('message', (message) => {
      this.emit('standardMessage', message);
    });

    this.normalizer.on('complete', (messageId, message) => {
      this.emit('standardComplete', message);
    });

    this.normalizer.on('update', (update) => {
      this.emit('stream', update);
    });

    this.normalizer.on('error', (error) => {
      this.emit('normalizerError', error);
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
   */
  async connect(): Promise<void> {
    if (this.state === AdapterState.CONNECTED) {
      return;
    }

    this.setState(AdapterState.CONNECTING);

    try {
      // 测试连接
      const connected = await this.client.testConnection();
      if (!connected) {
        throw new Error('Failed to connect to LLM');
      }

      this.setState(AdapterState.CONNECTED);
      logger.info(`${this.agent} adapter connected`, undefined, LogCategory.LLM);
    } catch (error: any) {
      this.setState(AdapterState.ERROR);
      logger.error(`${this.agent} adapter connection failed`, { error: error.message }, LogCategory.LLM);
      throw error;
    }
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
