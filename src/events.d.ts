/**
 * 事件系统
 * 提供事件发布/订阅机制，用于组件间通信
 */
import { EventType, AppEvent, EventListener } from './types';
/**
 * 事件发射器
 * 支持事件订阅、取消订阅、发布
 */
export declare class EventEmitter {
    private listeners;
    private allListeners;
    /**
     * 订阅特定类型的事件
     */
    on(type: EventType, listener: EventListener): () => void;
    /**
     * 订阅所有事件
     */
    onAll(listener: EventListener): () => void;
    /**
     * 取消订阅特定类型的事件
     */
    off(type: EventType, listener: EventListener): void;
    /**
     * 一次性订阅（触发后自动取消）
     */
    once(type: EventType, listener: EventListener): () => void;
    /**
     * 发布事件
     */
    emit(event: AppEvent): void;
    /**
     * 创建并发布事件的便捷方法
     */
    emitEvent(type: EventType, options?: Omit<AppEvent, 'type' | 'timestamp'>): void;
    /**
     * 清除所有监听器
     */
    clear(): void;
    /**
     * 清除特定类型的所有监听器
     */
    clearType(type: EventType): void;
    /**
     * 获取特定类型的监听器数量
     */
    listenerCount(type: EventType): number;
}
export declare const globalEventBus: EventEmitter;
export declare function emitEvent(type: EventType, options?: Omit<AppEvent, 'type' | 'timestamp'>): void;
export declare function onEvent(type: EventType, listener: EventListener): () => void;
//# sourceMappingURL=events.d.ts.map