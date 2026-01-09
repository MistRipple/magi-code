/**
 * 事件系统
 * 提供事件发布/订阅机制，用于组件间通信
 */

import { EventType, AppEvent, EventListener } from './types';

/**
 * 事件发射器
 * 支持事件订阅、取消订阅、发布
 */
export class EventEmitter {
  private listeners: Map<EventType, Set<EventListener>> = new Map();
  private allListeners: Set<EventListener> = new Set();

  /**
   * 订阅特定类型的事件
   */
  on(type: EventType, listener: EventListener): () => void {
    if (!this.listeners.has(type)) {
      this.listeners.set(type, new Set());
    }
    this.listeners.get(type)!.add(listener);

    // 返回取消订阅函数
    return () => this.off(type, listener);
  }

  /**
   * 订阅所有事件
   */
  onAll(listener: EventListener): () => void {
    this.allListeners.add(listener);
    return () => this.allListeners.delete(listener);
  }

  /**
   * 取消订阅特定类型的事件
   */
  off(type: EventType, listener: EventListener): void {
    this.listeners.get(type)?.delete(listener);
  }

  /**
   * 一次性订阅（触发后自动取消）
   */
  once(type: EventType, listener: EventListener): () => void {
    const wrapper: EventListener = (event) => {
      this.off(type, wrapper);
      listener(event);
    };
    return this.on(type, wrapper);
  }

  /**
   * 发布事件
   */
  emit(event: AppEvent): void {
    // 通知特定类型的监听器
    const typeListeners = this.listeners.get(event.type);
    if (typeListeners) {
      for (const listener of typeListeners) {
        try {
          listener(event);
        } catch (error) {
          console.error(`事件监听器错误 [${event.type}]:`, error);
        }
      }
    }

    // 通知全局监听器
    for (const listener of this.allListeners) {
      try {
        listener(event);
      } catch (error) {
        console.error(`全局事件监听器错误:`, error);
      }
    }
  }

  /**
   * 创建并发布事件的便捷方法
   */
  emitEvent(
    type: EventType,
    options: Omit<AppEvent, 'type' | 'timestamp'> = {}
  ): void {
    this.emit({
      type,
      timestamp: Date.now(),
      ...options,
    });
  }

  /**
   * 清除所有监听器
   */
  clear(): void {
    this.listeners.clear();
    this.allListeners.clear();
  }

  /**
   * 清除特定类型的所有监听器
   */
  clearType(type: EventType): void {
    this.listeners.delete(type);
  }

  /**
   * 获取特定类型的监听器数量
   */
  listenerCount(type: EventType): number {
    return this.listeners.get(type)?.size ?? 0;
  }
}

// 全局事件总线（单例）
export const globalEventBus = new EventEmitter();

// 便捷函数：发布事件
export function emitEvent(
  type: EventType,
  options: Omit<AppEvent, 'type' | 'timestamp'> = {}
): void {
  globalEventBus.emitEvent(type, options);
}

// 便捷函数：订阅事件
export function onEvent(type: EventType, listener: EventListener): () => void {
  return globalEventBus.on(type, listener);
}

