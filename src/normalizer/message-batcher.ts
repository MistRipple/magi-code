/**
 * Message Batcher - 消息批处理器
 * 
 * 优化 Webview 渲染性能，减少频繁的消息更新
 * 通过批量处理和节流来降低渲染压力
 */

import { logger, LogCategory } from '../logging';
import { StandardMessage, StreamUpdate } from '../protocol';

/** 批处理配置 */
export interface BatcherConfig {
  /** 批处理间隔（毫秒） */
  batchInterval: number;
  /** 最大批量大小 */
  maxBatchSize: number;
  /** 是否启用节流 */
  enableThrottle: boolean;
  /** 节流间隔（毫秒） */
  throttleInterval: number;
}

/** 批处理回调 */
export type BatchCallback = (updates: StreamUpdate[]) => void;

const DEFAULT_CONFIG: BatcherConfig = {
  batchInterval: 16, // ~60fps
  maxBatchSize: 50,
  enableThrottle: true,
  throttleInterval: 100,
};

/**
 * 消息批处理器
 */
export class MessageBatcher {
  private config: BatcherConfig;
  private pendingUpdates: Map<string, StreamUpdate[]> = new Map();
  private callbacks: Map<string, BatchCallback> = new Map();
  private batchTimer: NodeJS.Timeout | null = null;
  private lastFlushTime: Map<string, number> = new Map();
  private disposed = false;

  constructor(config?: Partial<BatcherConfig>) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 注册消息回调
   */
  registerCallback(messageId: string, callback: BatchCallback): void {
    this.callbacks.set(messageId, callback);
    this.pendingUpdates.set(messageId, []);
  }

  /**
   * 取消注册
   */
  unregisterCallback(messageId: string): void {
    this.callbacks.delete(messageId);
    this.pendingUpdates.delete(messageId);
    this.lastFlushTime.delete(messageId);
  }

  /**
   * 添加更新到批处理队列
   */
  addUpdate(update: StreamUpdate): void {
    if (this.disposed) return;

    const messageId = update.messageId;
    let updates = this.pendingUpdates.get(messageId);
    
    if (!updates) {
      updates = [];
      this.pendingUpdates.set(messageId, updates);
    }

    // 合并连续的 append 更新
    if (update.updateType === 'append' && updates.length > 0) {
      const lastUpdate = updates[updates.length - 1];
      if (lastUpdate.updateType === 'append' && lastUpdate.appendText && update.appendText) {
        lastUpdate.appendText += update.appendText;
        lastUpdate.timestamp = update.timestamp;
        this.scheduleFlush();
        return;
      }
    }

    updates.push(update);

    // 检查是否达到最大批量大小
    if (updates.length >= this.config.maxBatchSize) {
      this.flushMessage(messageId);
    } else {
      this.scheduleFlush();
    }
  }

  /**
   * 调度批量刷新
   */
  private scheduleFlush(): void {
    if (this.batchTimer) return;

    this.batchTimer = setTimeout(() => {
      this.batchTimer = null;
      this.flushAll();
    }, this.config.batchInterval);
  }

  /**
   * 刷新指定消息的更新
   */
  private flushMessage(messageId: string): void {
    const updates = this.pendingUpdates.get(messageId);
    const callback = this.callbacks.get(messageId);

    if (!updates || updates.length === 0 || !callback) return;

    // 节流检查
    if (this.config.enableThrottle) {
      const lastFlush = this.lastFlushTime.get(messageId) || 0;
      const now = Date.now();
      if (now - lastFlush < this.config.throttleInterval) {
        return; // 跳过此次刷新，等待下一个批处理周期
      }
      this.lastFlushTime.set(messageId, now);
    }

    // 清空队列并执行回调
    const batch = [...updates];
    updates.length = 0;
    
    try {
      callback(batch);
    } catch (error) {
      logger.error('规范化.消息批处理.回调_失败', error, LogCategory.SYSTEM);
    }
  }

  /**
   * 刷新所有待处理的更新
   */
  flushAll(): void {
    for (const messageId of this.pendingUpdates.keys()) {
      this.flushMessage(messageId);
    }
  }

  /**
   * 强制立即刷新（忽略节流）
   */
  forceFlush(messageId?: string): void {
    if (messageId) {
      this.lastFlushTime.delete(messageId);
      this.flushMessage(messageId);
    } else {
      this.lastFlushTime.clear();
      this.flushAll();
    }
  }

  /**
   * 销毁批处理器
   */
  dispose(): void {
    this.disposed = true;
    if (this.batchTimer) {
      clearTimeout(this.batchTimer);
      this.batchTimer = null;
    }
    this.flushAll();
    this.pendingUpdates.clear();
    this.callbacks.clear();
    this.lastFlushTime.clear();
  }
}
