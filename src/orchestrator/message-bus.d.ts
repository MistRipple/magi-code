/**
 * 消息总线 - 编排者与 Worker 之间的通信机制
 *
 * 核心功能：
 * - 消息发布/订阅
 * - 消息路由（定向/广播）
 * - 消息队列管理
 */
import { EventEmitter } from 'events';
import { BusMessage, MessageType, SubTask, ExecutionResult, WorkerInfo } from './protocols/types';
/**
 * 消息总线
 * 实现编排者与 Worker 之间的异步通信
 */
export declare class MessageBus extends EventEmitter {
    private subscribers;
    private messageHistory;
    private maxHistorySize;
    constructor();
    /**
     * 发布消息
     */
    publish(message: BusMessage): void;
    /**
     * 订阅消息（按 ID 或消息类型）
     */
    subscribe(key: string, callback: (message: BusMessage) => void): () => void;
    /**
     * 取消所有订阅
     */
    unsubscribeAll(key: string): void;
    /**
     * 获取消息历史
     */
    getHistory(filter?: {
        type?: MessageType;
        source?: string;
        target?: string;
    }): BusMessage[];
    /**
     * 清空消息历史
     */
    clearHistory(): void;
    /**
     * 销毁消息总线
     */
    dispose(): void;
    /**
     * 发送任务分发消息（编排者 -> Worker）
     */
    dispatchTask(source: string, target: string, taskId: string, subTask: SubTask, context?: string): void;
    /**
     * 发送任务取消消息（编排者 -> Worker）
     */
    cancelTask(source: string, target: string, taskId: string, subTaskId?: string, reason?: string): void;
    /**
     * 发送进度汇报消息（Worker -> 编排者）
     */
    reportProgress(source: string, target: string, taskId: string, subTaskId: string, status: 'started' | 'in_progress' | 'completed' | 'failed', options?: {
        progress?: number;
        message?: string;
        output?: string;
    }): void;
    /**
     * 发送任务完成消息（Worker -> 编排者）
     */
    reportTaskCompleted(source: string, target: string, result: ExecutionResult): void;
    /**
     * 发送任务失败消息（Worker -> 编排者）
     */
    reportTaskFailed(source: string, target: string, taskId: string, subTaskId: string, error: string, canRetry?: boolean): void;
    /**
     * 发送 Worker 就绪消息（Worker -> 编排者）
     */
    reportWorkerReady(source: string, target: string, workerInfo: WorkerInfo): void;
    /**
     * 发送编排者命令（编排者 -> 所有 Worker）
     */
    broadcastCommand(source: string, command: 'pause_all' | 'resume_all' | 'cancel_all' | 'status_check'): void;
}
/** 全局消息总线实例 */
export declare const globalMessageBus: MessageBus;
//# sourceMappingURL=message-bus.d.ts.map