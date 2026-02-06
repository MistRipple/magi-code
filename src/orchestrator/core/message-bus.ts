/**
 * MessageBus - 消息传输层
 *
 * 职责单一：事件发射。作为 L2 中枢层的传输组件，负责将消息发射到 L3 桥接层。
 * - 继承 EventEmitter，提供类型安全的事件订阅
 * - safeEmit 方法防止监听器异常导致消息流中断
 * - 判断消息终态，自动发射 unified:complete 事件
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import type { StandardMessage, StreamUpdate, MessageSource } from '../../protocol/message-protocol';
import { MessageLifecycle } from '../../protocol/message-protocol';

/** 处理状态 */
export interface ProcessingState {
  isProcessing: boolean;
  source: MessageSource | null;
  agent: string | null;
  startedAt: number | null;
}

/** 广播消息数据 */
export interface BroadcastData {
  message: StandardMessage;
  target?: string;
  timestamp: number;
}

/** MessageBus 事件类型映射 */
export interface MessageBusEvents {
  'unified:message': (message: StandardMessage) => void;
  'unified:update': (update: StreamUpdate) => void;
  'unified:complete': (message: StandardMessage) => void;
  'broadcast': (data: BroadcastData) => void;
  'processingStateChanged': (state: ProcessingState) => void;
}

/** MessageBus 配置 */
export interface MessageBusConfig {
  debug: boolean;
}

const DEFAULT_CONFIG: MessageBusConfig = {
  debug: false,
};

/**
 * MessageBus - 消息传输层
 * 继承 EventEmitter，职责单一：事件发射
 */
export class MessageBus extends EventEmitter {
  private config: MessageBusConfig;

  constructor(config?: Partial<MessageBusConfig>) {
    super();
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /** 发射标准消息事件，终态时自动发射 unified:complete */
  emitMessage(message: StandardMessage): boolean {
    const success = this.safeEmit('unified:message', message);
    if (success && this.isTerminal(message.lifecycle)) {
      this.safeEmit('unified:complete', message);
    }
    return success;
  }

  /** 发射流式更新事件 */
  emitUpdate(update: StreamUpdate): boolean {
    return this.safeEmit('unified:update', update);
  }

  /** 发射广播消息事件 */
  emitBroadcast(data: BroadcastData): boolean {
    return this.safeEmit('broadcast', data);
  }

  /** 发射处理状态变化事件 */
  emitProcessingStateChanged(state: ProcessingState): boolean {
    return this.safeEmit('processingStateChanged', state);
  }

  /** 判断是否为终态生命周期（COMPLETED/FAILED/CANCELLED） */
  isTerminal(lifecycle: MessageLifecycle): boolean {
    return (
      lifecycle === MessageLifecycle.COMPLETED ||
      lifecycle === MessageLifecycle.FAILED ||
      lifecycle === MessageLifecycle.CANCELLED
    );
  }

  /**
   * 安全的事件发射方法
   * 捕获监听器异常，确保消息流不中断；debug 模式下会重新抛出
   */
  safeEmit(event: keyof MessageBusEvents, data: unknown): boolean {
    try {
      return this.emit(event, data);
    } catch (error) {
      logger.error('MessageBus.safeEmit.failed', {
        event,
        error: error instanceof Error ? error.message : String(error),
        stack: error instanceof Error ? error.stack : undefined,
        messageId: (data as StandardMessage)?.id,
      }, LogCategory.SYSTEM);

      if (this.config.debug) {
        throw error;
      }
      return false;
    }
  }
}
