import { logger, LogCategory } from '../logging';

/**
 * TimeoutChecker - 超时检测器
 *
 * 职责：
 * - 定期检查 Task/SubTask 是否超时
 * - 触发超时回调
 * - 支持动态添加/移除监控项
 */

export interface TimeoutItem {
  id: string;
  timeoutAt: number;
  callback: () => void;
}

/**
 * 超时检测器
 * 用于自动检测和处理任务超时
 */
export class TimeoutChecker {
  private items: Map<string, TimeoutItem> = new Map();
  private timer: NodeJS.Timeout | null = null;
  private checkInterval: number = 1000; // 默认 1 秒检查一次

  constructor(checkInterval?: number) {
    if (checkInterval !== undefined && checkInterval > 0) {
      this.checkInterval = checkInterval;
    }
  }

  /**
   * 添加超时监控项
   */
  add(id: string, timeoutAt: number, callback: () => void): void {
    this.items.set(id, { id, timeoutAt, callback });
    this.ensureRunning();
  }

  /**
   * 移除超时监控项
   */
  remove(id: string): void {
    this.items.delete(id);
    if (this.items.size === 0) {
      this.stop();
    }
  }

  /**
   * 更新超时时间
   */
  update(id: string, timeoutAt: number): void {
    const item = this.items.get(id);
    if (item) {
      item.timeoutAt = timeoutAt;
    }
  }

  /**
   * 清空所有监控项
   */
  clear(): void {
    this.items.clear();
    this.stop();
  }

  /**
   * 获取监控项数量
   */
  size(): number {
    return this.items.size;
  }

  /**
   * 检查指定项是否存在
   */
  has(id: string): boolean {
    return this.items.has(id);
  }

  /**
   * 获取所有监控项 ID
   */
  getIds(): string[] {
    return Array.from(this.items.keys());
  }

  /**
   * 启动定时检查
   */
  private ensureRunning(): void {
    if (!this.timer) {
      this.timer = setInterval(() => this.check(), this.checkInterval);
    }
  }

  /**
   * 停止定时检查
   */
  private stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /**
   * 检查超时
   */
  private check(): void {
    const now = Date.now();
    const expired: string[] = [];

    for (const [id, item] of this.items) {
      if (now >= item.timeoutAt) {
        expired.push(id);
        try {
          item.callback();
        } catch (error) {
          logger.error('任务.超时.回调_失败', { id, error }, LogCategory.TASK);
        }
      }
    }

    // 移除已超时的项
    for (const id of expired) {
      this.items.delete(id);
    }

    // 如果没有监控项了，停止定时器
    if (this.items.size === 0) {
      this.stop();
    }
  }

  /**
   * 销毁检查器
   */
  destroy(): void {
    this.clear();
  }
}
