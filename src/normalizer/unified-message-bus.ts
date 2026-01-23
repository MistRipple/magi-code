/**
 * UnifiedMessageBus - 统一消息总线
 *
 * 核心职责：
 * 1. 作为所有消息的唯一出口（单一消息源）
 * 2. 统一去重逻辑（唯一去重点）
 * 3. 消息批处理和节流
 * 4. 处理状态管理（isProcessing 的权威来源）
 *
 * 设计原则：
 * - 所有消息通过 MessageBus 发送
 * - 前端只负责渲染，不做去重
 * - 后端是状态权威
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../logging';
import {
  StandardMessage,
  StreamUpdate,
  MessageLifecycle,
  MessageSource,
} from '../protocol';

// ============================================================================
// 类型定义
// ============================================================================

/** 消息总线配置 */
export interface MessageBusConfig {
  /** 是否启用（特性开关） */
  enabled: boolean;
  /** 流式消息最小发送间隔（毫秒） */
  minStreamInterval: number;
  /** 批处理间隔（毫秒） */
  batchInterval: number;
  /** 消息历史保留时间（毫秒） */
  retentionTime: number;
  /** 调试模式 */
  debug: boolean;
}

/** 消息状态 */
interface MessageState {
  message: StandardMessage;
  lastSentAt: number;
  lastStreamAt: number;
  completed: boolean;
}

/** 处理状态 */
export interface ProcessingState {
  isProcessing: boolean;
  source: MessageSource | null;
  agent: string | null;  // ✅ 使用 agent 替代旧字段
  startedAt: number | null;
}

/** 消息总线事件 */
export interface MessageBusEvents {
  /** 标准消息（发送到前端） */
  message: (message: StandardMessage) => void;
  /** 流式更新（发送到前端） */
  update: (update: StreamUpdate) => void;
  /** 消息完成（发送到前端） */
  complete: (message: StandardMessage) => void;
  /** 处理状态变化 */
  processingStateChanged: (state: ProcessingState) => void;
}

// ============================================================================
// 默认配置
// ============================================================================

const DEFAULT_CONFIG: MessageBusConfig = {
  enabled: true,
  minStreamInterval: 100,
  batchInterval: 50,
  retentionTime: 5 * 60 * 1000,
  debug: false,
};

// ============================================================================
// UnifiedMessageBus 实现
// ============================================================================

export class UnifiedMessageBus extends EventEmitter {
  private config: MessageBusConfig;
  private messageStates: Map<string, MessageState> = new Map();
  private processingState: ProcessingState = {
    isProcessing: false,
    source: null,
    agent: null,  // ✅ 使用 agent
    startedAt: null,
  };
  private activeMessageIds: Set<string> = new Set();
  private cleanupTimer: NodeJS.Timeout | null = null;

  constructor(config?: Partial<MessageBusConfig>) {
    super();
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.startCleanupTimer();
  }

  // ==========================================================================
  // 公共 API
  // ==========================================================================

  /**
   * 发送标准消息（唯一入口）
   */
  sendMessage(message: StandardMessage): boolean {
    if (!this.config.enabled) {
      this.emit('message', message);
      return true;
    }

    const { id, lifecycle } = message;
    const now = Date.now();
    const existingState = this.messageStates.get(id);

    // 1. STARTED 消息：总是发送
    if (lifecycle === MessageLifecycle.STARTED) {
      this.recordMessage(message, now);
      this.updateProcessingState(true, message.source, message.agent);  // ✅ 使用 agent
      this.emit('message', message);
      this.debug('发送消息 [STARTED]', id);
      return true;
    }

    // 2. 新消息：发送
    if (!existingState) {
      this.recordMessage(message, now);
      if (lifecycle === MessageLifecycle.STREAMING) {
        this.updateProcessingState(true, message.source, message.agent);  // ✅ 使用 agent
      }
      this.emit('message', message);
      this.debug('发送消息 [NEW]', id);
      return true;
    }

    // 3. 已完成的消息：不再发送
    if (existingState.completed) {
      this.debug('跳过消息 [COMPLETED]', id);
      return false;
    }

    // 4. STREAMING 消息：检查发送间隔
    if (lifecycle === MessageLifecycle.STREAMING) {
      const timeSinceLastStream = now - existingState.lastStreamAt;
      if (timeSinceLastStream < this.config.minStreamInterval) {
        this.debug('跳过消息 [THROTTLE]', id);
        return false;
      }
      existingState.lastStreamAt = now;
      existingState.message = message;
      this.emit('message', message);
      this.debug('发送消息 [STREAMING]', id);
      return true;
    }

    // 5. COMPLETED/FAILED/CANCELLED：标记完成，发送
    if (this.isTerminalLifecycle(lifecycle)) {
      this.completeMessage(id, message, now);
      this.emit('complete', message);
      this.checkAndUpdateProcessingState();
      this.debug('发送消息 [COMPLETE]', id);
      return true;
    }

    return true;
  }

  /**
   * 发送流式更新
   */
  sendUpdate(update: StreamUpdate): boolean {
    if (!this.config.enabled) {
      this.emit('update', update);
      return true;
    }

    const now = Date.now();
    const state = this.messageStates.get(update.messageId);

    if (!state) {
      this.debug('跳过更新 [NO_STATE]', update.messageId);
      return false;
    }

    if (state.completed) {
      this.debug('跳过更新 [COMPLETED]', update.messageId);
      return false;
    }

    const timeSinceLastStream = now - state.lastStreamAt;
    if (timeSinceLastStream < this.config.minStreamInterval) {
      this.debug('跳过更新 [THROTTLE]', update.messageId);
      return false;
    }

    state.lastStreamAt = now;
    this.emit('update', update);
    this.debug('发送更新', update.messageId);
    return true;
  }

  /**
   * 获取当前处理状态
   */
  getProcessingState(): ProcessingState {
    return { ...this.processingState };
  }

  /**
   * 强制设置处理状态（用于外部控制，如用户中断）
   */
  forceProcessingState(isProcessing: boolean): void {
    if (!isProcessing) {
      this.activeMessageIds.clear();
    }
    this.updateProcessingState(isProcessing, null, null);
  }

  /**
   * 清理资源
   */
  dispose(): void {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
    this.messageStates.clear();
    this.activeMessageIds.clear();
    this.removeAllListeners();
  }

  // ==========================================================================
  // 私有方法
  // ==========================================================================

  private recordMessage(message: StandardMessage, timestamp: number): void {
    this.messageStates.set(message.id, {
      message,
      lastSentAt: timestamp,
      lastStreamAt: timestamp,
      completed: false,
    });
    this.activeMessageIds.add(message.id);
  }

  private completeMessage(id: string, message: StandardMessage, timestamp: number): void {
    const state = this.messageStates.get(id);
    if (state) {
      state.message = message;
      state.lastSentAt = timestamp;
      state.completed = true;
    }
    this.activeMessageIds.delete(id);
  }

  private isTerminalLifecycle(lifecycle: MessageLifecycle): boolean {
    return (
      lifecycle === MessageLifecycle.COMPLETED ||
      lifecycle === MessageLifecycle.FAILED ||
      lifecycle === MessageLifecycle.CANCELLED
    );
  }

  private updateProcessingState(isProcessing: boolean, source: MessageSource | null, agent: string | null): void {  // ✅ 使用 agent
    const prev = this.processingState.isProcessing;
    this.processingState = {
      isProcessing,
      source: isProcessing ? source : null,
      agent: isProcessing ? agent : null,  // ✅ 使用 agent
      startedAt: isProcessing ? (this.processingState.startedAt || Date.now()) : null,
    };
    if (prev !== isProcessing) {
      this.emit('processingStateChanged', this.getProcessingState());
    }
  }

  private checkAndUpdateProcessingState(): void {
    if (this.activeMessageIds.size === 0) {
      this.updateProcessingState(false, null, null);
    }
  }

  private startCleanupTimer(): void {
    this.cleanupTimer = setInterval(() => {
      const now = Date.now();
      const expireTime = now - this.config.retentionTime;
      for (const [id, state] of this.messageStates) {
        if (state.completed && state.lastSentAt < expireTime) {
          this.messageStates.delete(id);
        }
      }
    }, 60 * 1000);
  }

  private debug(action: string, messageId: string): void {
    if (this.config.debug) {
      logger.debug('消息总线.' + action, { messageId }, LogCategory.SYSTEM);
    }
  }

  // 类型安全的事件方法
  on<K extends keyof MessageBusEvents>(event: K, listener: MessageBusEvents[K]): this {
    return super.on(event, listener);
  }

  off<K extends keyof MessageBusEvents>(event: K, listener: MessageBusEvents[K]): this {
    return super.off(event, listener);
  }

  emit<K extends keyof MessageBusEvents>(event: K, ...args: Parameters<MessageBusEvents[K]>): boolean {
    return super.emit(event, ...args);
  }
}
