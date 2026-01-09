/**
 * 消息总线 - 编排者与 Worker 之间的通信机制
 * 
 * 核心功能：
 * - 消息发布/订阅
 * - 消息路由（定向/广播）
 * - 消息队列管理
 */

import { EventEmitter } from 'events';
import {
  BusMessage,
  MessageType,
  MessageBusEvents,
  TaskDispatchMessage,
  TaskCancelMessage,
  ProgressReportMessage,
  TaskCompletedMessage,
  TaskFailedMessage,
  WorkerReadyMessage,
  OrchestratorCommandMessage,
  SubTask,
  ExecutionResult,
  WorkerInfo,
} from './protocols/types';

/** 生成唯一消息 ID */
function generateMessageId(): string {
  return `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

/**
 * 消息总线
 * 实现编排者与 Worker 之间的异步通信
 */
export class MessageBus extends EventEmitter {
  private subscribers: Map<string, Set<(message: BusMessage) => void>> = new Map();
  private messageHistory: BusMessage[] = [];
  private maxHistorySize: number = 1000;

  constructor() {
    super();
  }

  /**
   * 发布消息
   */
  publish(message: BusMessage): void {
    // 记录消息历史
    this.messageHistory.push(message);
    if (this.messageHistory.length > this.maxHistorySize) {
      this.messageHistory.shift();
    }

    // 触发全局消息事件
    this.emit('message', message);

    // 如果有目标，发送定向消息
    if (message.target) {
      const targetSubscribers = this.subscribers.get(message.target);
      if (targetSubscribers) {
        targetSubscribers.forEach(callback => callback(message));
      }
    }

    // 发送给订阅该消息类型的所有订阅者
    const typeSubscribers = this.subscribers.get(message.type);
    if (typeSubscribers) {
      typeSubscribers.forEach(callback => callback(message));
    }
  }

  /**
   * 订阅消息（按 ID 或消息类型）
   */
  subscribe(key: string, callback: (message: BusMessage) => void): () => void {
    if (!this.subscribers.has(key)) {
      this.subscribers.set(key, new Set());
    }
    this.subscribers.get(key)!.add(callback);

    // 返回取消订阅函数
    return () => {
      const subs = this.subscribers.get(key);
      if (subs) {
        subs.delete(callback);
        if (subs.size === 0) {
          this.subscribers.delete(key);
        }
      }
    };
  }

  /**
   * 取消所有订阅
   */
  unsubscribeAll(key: string): void {
    this.subscribers.delete(key);
  }

  /**
   * 获取消息历史
   */
  getHistory(filter?: { type?: MessageType; source?: string; target?: string }): BusMessage[] {
    if (!filter) {
      return [...this.messageHistory];
    }

    return this.messageHistory.filter(msg => {
      if (filter.type && msg.type !== filter.type) return false;
      if (filter.source && msg.source !== filter.source) return false;
      if (filter.target && msg.target !== filter.target) return false;
      return true;
    });
  }

  /**
   * 清空消息历史
   */
  clearHistory(): void {
    this.messageHistory = [];
  }

  /**
   * 销毁消息总线
   */
  dispose(): void {
    this.subscribers.clear();
    this.messageHistory = [];
    this.removeAllListeners();
  }

  // =========================================================================
  // 便捷方法：创建并发布特定类型的消息
  // =========================================================================

  /**
   * 发送任务分发消息（编排者 -> Worker）
   */
  dispatchTask(source: string, target: string, taskId: string, subTask: SubTask, context?: string): void {
    const message: TaskDispatchMessage = {
      id: generateMessageId(),
      type: 'task_dispatch',
      timestamp: Date.now(),
      source,
      target,
      payload: { taskId, subTask, context },
    };
    this.publish(message);
  }

  /**
   * 发送任务取消消息（编排者 -> Worker）
   */
  cancelTask(source: string, target: string, taskId: string, subTaskId?: string, reason?: string): void {
    const message: TaskCancelMessage = {
      id: generateMessageId(),
      type: 'task_cancel',
      timestamp: Date.now(),
      source,
      target,
      payload: { taskId, subTaskId, reason },
    };
    this.publish(message);
  }

  /**
   * 发送进度汇报消息（Worker -> 编排者）
   */
  reportProgress(
    source: string,
    target: string,
    taskId: string,
    subTaskId: string,
    status: 'started' | 'in_progress' | 'completed' | 'failed',
    options?: { progress?: number; message?: string; output?: string }
  ): void {
    const message: ProgressReportMessage = {
      id: generateMessageId(),
      type: 'progress_report',
      timestamp: Date.now(),
      source,
      target,
      payload: { taskId, subTaskId, status, ...options },
    };
    this.publish(message);
  }

  /**
   * 发送任务完成消息（Worker -> 编排者）
   */
  reportTaskCompleted(source: string, target: string, result: ExecutionResult): void {
    const message: TaskCompletedMessage = {
      id: generateMessageId(),
      type: 'task_completed',
      timestamp: Date.now(),
      source,
      target,
      payload: { result },
    };
    this.publish(message);
  }

  /**
   * 发送任务失败消息（Worker -> 编排者）
   */
  reportTaskFailed(
    source: string,
    target: string,
    taskId: string,
    subTaskId: string,
    error: string,
    canRetry: boolean = true
  ): void {
    const message: TaskFailedMessage = {
      id: generateMessageId(),
      type: 'task_failed',
      timestamp: Date.now(),
      source,
      target,
      payload: { taskId, subTaskId, error, canRetry },
    };
    this.publish(message);
  }

  /**
   * 发送 Worker 就绪消息（Worker -> 编排者）
   */
  reportWorkerReady(source: string, target: string, workerInfo: WorkerInfo): void {
    const message: WorkerReadyMessage = {
      id: generateMessageId(),
      type: 'worker_ready',
      timestamp: Date.now(),
      source,
      target,
      payload: { workerInfo },
    };
    this.publish(message);
  }

  /**
   * 发送编排者命令（编排者 -> 所有 Worker）
   */
  broadcastCommand(
    source: string,
    command: 'pause_all' | 'resume_all' | 'cancel_all' | 'status_check'
  ): void {
    const message: OrchestratorCommandMessage = {
      id: generateMessageId(),
      type: 'orchestrator_command',
      timestamp: Date.now(),
      source,
      payload: { command },
    };
    this.publish(message);
  }
}

/** 全局消息总线实例 */
export const globalMessageBus = new MessageBus();
