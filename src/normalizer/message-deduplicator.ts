/**
 * MessageDeduplicator - 消息去重与生命周期管理
 *
 * 职责：
 * 1. 基于消息 ID 和生命周期进行去重
 * 2. 流式消息的合并与更新
 * 3. 确保消息顺序和完整性
 * 4. 防止消息覆盖和重复发送
 *
 * 去重规则：
 * - STARTED 消息：总是发送
 * - STREAMING 消息：合并更新，不重复发送
 * - COMPLETED 消息：更新状态，不重复发送
 * - 不同 source 的消息严格隔离
 */

import { logger, LogCategory } from '../logging';
import { StandardMessage, MessageLifecycle, MessageSource, StreamUpdate } from '../protocol';

/** 消息状态 */
interface MessageState {
  /** 消息对象 */
  message: StandardMessage;
  /** 已发送次数 */
  sentCount: number;
  /** 最后发送时间 */
  lastSentAt: number;
  /** 最后 STREAMING 消息发送时间 */
  lastStreamAt: number;
  /** 是否完成 */
  completed: boolean;
}

/** 去重配置 */
export interface DeduplicationConfig {
  /** 是否启用去重 */
  enabled: boolean;
  /** 流式消息最小发送间隔（毫秒） */
  minStreamInterval: number;
  /** 消息历史保留时间（毫秒） */
  retentionTime: number;
  /** 最大历史记录数 */
  maxHistorySize: number;
}

/** 默认配置 */
const DEFAULT_CONFIG: DeduplicationConfig = {
  enabled: true,
  minStreamInterval: 100, // 100ms 最小间隔
  retentionTime: 5 * 60 * 1000, // 5 分钟
  maxHistorySize: 1000,
};

/**
 * 消息去重器
 */
export class MessageDeduplicator {
  private config: DeduplicationConfig;
  /** 消息状态表: messageId -> MessageState */
  private messageStates: Map<string, MessageState> = new Map();
  /** 按 source 分组的消息 ID 列表 */
  private messagesBySource: Map<MessageSource, string[]> = new Map();
  /** 仅更新时的时间戳缓存（用于缺少完整 message 的 StreamUpdate） */
  private streamUpdateTimes: Map<string, number> = new Map();

  constructor(config?: Partial<DeduplicationConfig>) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  /**
   * 处理消息，返回是否应该发送
   *
   * @returns true 表示应该发送，false 表示应该跳过
   */
  shouldSend(message: StandardMessage): boolean {
    if (!this.config.enabled) {
      return true; // 禁用去重时总是发送
    }

    const { id, lifecycle, source } = message;
    const existingState = this.messageStates.get(id);
    const now = Date.now();

    // 1. STARTED 消息：总是发送（新消息开始）
    if (lifecycle === MessageLifecycle.STARTED) {
      this.recordMessage(message, now, true);
      return true;
    }

    // 2. 新消息（没有记录）：发送
    if (!existingState) {
      this.recordMessage(message, now, true);
      return true;
    }

    // 3. 已完成的消息：不再发送
    if (existingState.completed) {
      logger.warn('规范化.消息去重.跳过_完成', { id }, LogCategory.SYSTEM);
      return false;
    }

    // 4. STREAMING 消息：检查发送间隔
    if (lifecycle === MessageLifecycle.STREAMING) {
      const timeSinceLastStream = now - existingState.lastStreamAt;
      if (timeSinceLastStream < this.config.minStreamInterval) {
        // 间隔太短，跳过
        this.updateMessage(message, false);
        return false;
      }
      // 间隔足够，发送更新
      this.updateMessage(message, true);
      return true;
    }

    // 5. COMPLETED/FAILED/INTERRUPTED 消息：标记完成，发送一次
    if (
      lifecycle === MessageLifecycle.COMPLETED ||
      lifecycle === MessageLifecycle.FAILED ||
      lifecycle === MessageLifecycle.CANCELLED
    ) {
      this.completeMessage(message, now);
      return true;
    }

    // 默认：发送
    return true;
  }

  /**
   * 处理流式更新（缺少完整 message 的场景）
   */
  shouldSendUpdate(update: StreamUpdate): boolean {
    if (!this.config.enabled) {
      return true;
    }

    const now = Date.now();
    const state = this.messageStates.get(update.messageId);
    const lastStreamAt = state?.lastStreamAt ?? this.streamUpdateTimes.get(update.messageId) ?? 0;

    if (now - lastStreamAt < this.config.minStreamInterval) {
      return false;
    }

    if (state) {
      state.lastStreamAt = now;
    } else {
      this.streamUpdateTimes.set(update.messageId, now);
    }

    return true;
  }

  /**
   * 记录新消息
   */
  private recordMessage(message: StandardMessage, timestamp: number, sent: boolean): void {
    this.messageStates.set(message.id, {
      message,
      sentCount: sent ? 1 : 0,
      lastSentAt: sent ? timestamp : 0,
      lastStreamAt: 0, // 初始化为 0,表示还没有 STREAMING 消息
      completed: false,
    });

    // 按 source 分组
    if (!this.messagesBySource.has(message.source)) {
      this.messagesBySource.set(message.source, []);
    }
    this.messagesBySource.get(message.source)!.push(message.id);

    // 清理过期消息
    this.cleanupOldMessages();
  }

  /**
   * 更新消息
   */
  private updateMessage(message: StandardMessage, sent: boolean): void {
    const state = this.messageStates.get(message.id);
    if (state) {
      state.message = message;
      if (sent) {
        state.sentCount++;
        state.lastSentAt = Date.now();
        // 如果是 STREAMING 消息,更新 lastStreamAt
        if (message.lifecycle === MessageLifecycle.STREAMING) {
          state.lastStreamAt = Date.now();
        }
      }
    }
  }

  /**
   * 标记消息完成
   */
  private completeMessage(message: StandardMessage, timestamp: number): void {
    const state = this.messageStates.get(message.id);
    if (state) {
      state.message = message;
      state.completed = true;
      state.sentCount++;
      state.lastSentAt = timestamp;
    } else {
      // 没有记录，直接标记完成
      this.messageStates.set(message.id, {
        message,
        sentCount: 1,
        lastSentAt: timestamp,
        lastStreamAt: 0,
        completed: true,
      });
    }
  }

  /**
   * 清理过期消息
   */
  private cleanupOldMessages(): void {
    const now = Date.now();
    const toDelete: string[] = [];

    for (const [id, state] of this.messageStates) {
      // 删除条件：已完成 且 超过保留时间
      if (state.completed && now - state.lastSentAt > this.config.retentionTime) {
        toDelete.push(id);
      }
    }

    // 删除过期消息
    for (const id of toDelete) {
      this.messageStates.delete(id);
      this.streamUpdateTimes.delete(id);
      // 从 source 分组中删除
      for (const ids of this.messagesBySource.values()) {
        const index = ids.indexOf(id);
        if (index !== -1) {
          ids.splice(index, 1);
        }
      }
    }

    // 限制历史大小
    if (this.messageStates.size > this.config.maxHistorySize) {
      const excess = this.messageStates.size - this.config.maxHistorySize;
      const oldestIds = Array.from(this.messageStates.entries())
        .filter(([_, state]) => state.completed)
        .sort((a, b) => a[1].lastSentAt - b[1].lastSentAt)
        .slice(0, excess)
        .map(([id]) => id);

      for (const id of oldestIds) {
        this.messageStates.delete(id);
        this.streamUpdateTimes.delete(id);
      }
    }
  }

  /**
   * 获取某个 source 的所有消息
   */
  getMessagesBySource(source: MessageSource): StandardMessage[] {
    const ids = this.messagesBySource.get(source) || [];
    return ids
      .map(id => this.messageStates.get(id)?.message)
      .filter((msg): msg is StandardMessage => msg !== undefined);
  }

  /**
   * 获取消息状态
   */
  getMessageState(id: string): MessageState | undefined {
    return this.messageStates.get(id);
  }

  /**
   * 重置去重器
   */
  reset(): void {
    this.messageStates.clear();
    this.messagesBySource.clear();
    this.streamUpdateTimes.clear();
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<DeduplicationConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /**
   * 获取统计信息
   */
  getStats() {
    return {
      totalMessages: this.messageStates.size,
      completedMessages: Array.from(this.messageStates.values()).filter(s => s.completed).length,
      sourceBreakdown: Object.fromEntries(
        Array.from(this.messagesBySource.entries()).map(([source, ids]) => [source, ids.length])
      ),
    };
  }
}
