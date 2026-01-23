/**
 * MessageBridge - 消息桥接层
 *
 * 连接 IAdapterFactory 和 Webview，负责：
 * 1. 接收 IAdapterFactory 的标准消息事件
 * 2. 转发标准消息到 Webview
 * 3. 管理消息生命周期
 *
 * 这是消息标准化架构的核心组件
 */

import { logger, LogCategory } from '../logging';
import { EventEmitter } from 'events';
import type { WorkerSlot } from '../types';
import type { IAdapterFactory } from '../adapters/adapter-factory-interface';
import {
  StandardMessage,
  StreamUpdate,
} from '../protocol';

/**
 * MessageBridge 事件
 */
export interface MessageBridgeEvents {
  /** 新的标准消息 */
  message: (message: StandardMessage) => void;
  /** 消息更新（流式） */
  update: (update: StreamUpdate) => void;
  /** 消息完成 */
  complete: (message: StandardMessage) => void;
  /** 错误 */
  error: (error: Error, worker?: WorkerSlot) => void;
}

/**
 * MessageBridge 配置
 */
export interface MessageBridgeConfig {
  /** 是否启用调试日志 */
  debug?: boolean;
  /** 流式消息超时时间（毫秒） */
  streamTimeout?: number;
}

/**
 * 消息桥接层
 * 将模型原始输出转换为标准消息格式
 */
export class MessageBridge extends EventEmitter {
  private factory: IAdapterFactory;
  private config: MessageBridgeConfig;

  constructor(factory: IAdapterFactory, config?: MessageBridgeConfig) {
    super();
    this.factory = factory;
    this.config = {
      debug: false,
      streamTimeout: 5 * 60 * 1000, // 5 分钟
      ...config,
    };

    this.setupFactoryListeners();
  }

  /**
   * 设置工厂事件监听
   */
  private setupFactoryListeners(): void {
    // 监听标准消息事件
    this.factory.on('standardMessage', (message: StandardMessage) => {
      this.emit('message', message);
      this.debug(`[MessageBridge] 标准消息: ${message.id} [${message.lifecycle}]`);
    });

    // 监听标准更新事件
    this.factory.on('standardUpdate', (update: StreamUpdate) => {
      this.emit('update', update);
      this.debug(`[MessageBridge] 标准更新: ${update.messageId}`);
    });

    // 监听标准完成事件
    this.factory.on('standardComplete', (message: StandardMessage) => {
      this.emit('complete', message);
      this.debug(`[MessageBridge] 标准完成: ${message.id}`);
    });

    // 监听错误
    this.factory.on('error', ({ type, error }: any) => {
      this.handleError(type, error);
    });
  }

  /**
   * 处理错误
   */
  private handleError(worker: WorkerSlot, error: Error): void {
    this.emit('error', error, worker);
  }

  /**
   * 清理资源
   */
  dispose(): void {
    this.removeAllListeners();
  }

  /**
   * 调试日志
   */
  private debug(message: string, ...args: unknown[]): void {
    if (this.config.debug) {
      logger.debug('规范化.消息桥.调试', { message, args }, LogCategory.SYSTEM);
    }
  }

  // 事件类型安全
  on<K extends keyof MessageBridgeEvents>(event: K, listener: MessageBridgeEvents[K]): this {
    return super.on(event, listener);
  }

  off<K extends keyof MessageBridgeEvents>(event: K, listener: MessageBridgeEvents[K]): this {
    return super.off(event, listener);
  }

  emit<K extends keyof MessageBridgeEvents>(event: K, ...args: Parameters<MessageBridgeEvents[K]>): boolean {
    return super.emit(event, ...args);
  }
}
