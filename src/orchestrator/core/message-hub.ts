/**
 * MessageHub - 统一消息出口
 *
 * 设计原则（来自 orchestration-unified-design.md 第 7.4 节）：
 * - 所有 UI 消息统一走 MessageHub
 * - 主对话区只承载编排者叙事与关键里程碑
 * - Worker 输出只在各自 Tab 显示
 *
 * 消息来源归类：
 * - orchestrator: 主对话区（编排者叙事、分配、总结）
 * - worker: Worker Tab（只在对应 Tab 输出）
 * - system: 主对话区（系统通知、阶段变化）
 * - subTaskCard: 主对话区（子任务完成摘要卡片）
 */

import { EventEmitter } from 'events';
import type { WorkerSlot } from '../../types';
import type { StandardMessage, MessageMetadata, ContentBlock, MessageSource, StreamUpdate } from '../../protocol/message-protocol';
import { MessageType, MessageLifecycle, createStandardMessage, generateMessageId } from '../../protocol/message-protocol';
import type { AgentType } from '../../types/agent-types';

/**
 * 子任务视图 - 用于 SubTaskCard 消息
 */
export interface SubTaskView {
  id: string;
  title: string;
  status: 'pending' | 'running' | 'completed' | 'failed';
  worker: WorkerSlot;
  summary?: string;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}

/**
 * 进度消息数据
 */
export interface ProgressData {
  phase: string;
  content: string;
  percentage?: number;
  metadata?: MessageMetadata;
}

/**
 * 结果消息数据
 */
export interface ResultData {
  content: string;
  success: boolean;
  metadata?: MessageMetadata;
}

/**
 * 错误消息数据
 */
export interface ErrorData {
  error: string;
  details?: Record<string, unknown>;
  recoverable?: boolean;
}

/**
 * Worker 输出数据
 */
export interface WorkerOutputData {
  worker: WorkerSlot;
  content: string;
  blocks?: ContentBlock[];
  metadata?: MessageMetadata;
}

/**
 * MessageHub 事件类型
 */
export interface MessageHubEvents {
  /** 编排者消息（主对话区） */
  'orchestrator:message': (message: StandardMessage) => void;
  /** Worker 输出（路由到对应 Worker Tab） */
  'worker:output': (data: WorkerOutputData & { message: StandardMessage }) => void;
  /** 子任务卡片（主对话区） */
  'subTaskCard': (subTask: SubTaskView) => void;
  /** 进度更新 */
  'progress': (data: ProgressData & { message: StandardMessage }) => void;
  /** 结果消息 */
  'result': (data: ResultData & { message: StandardMessage }) => void;
  /** 错误消息 */
  'error': (data: ErrorData & { message: StandardMessage }) => void;
  /** 系统通知 */
  'system:notice': (message: StandardMessage) => void;
  /** 标准消息（来自 LLM/内部流） */
  'standard:message': (message: StandardMessage) => void;
  /** 标准流式更新 */
  'standard:update': (update: StreamUpdate) => void;
  /** 标准完成消息 */
  'standard:complete': (message: StandardMessage) => void;
}

/**
 * MessageHub - 统一消息出口
 *
 * 提供语义化的消息发送 API，所有 UI 消息都通过此类发送：
 * - progress(): 进度消息
 * - result(): 结果消息
 * - workerOutput(): Worker 输出（路由到对应 Tab）
 * - subTaskCard(): 子任务卡片（显示在主对话区）
 * - error(): 错误消息
 */
export class MessageHub extends EventEmitter {
  private traceId: string;

  constructor(traceId?: string) {
    super();
    this.traceId = traceId || this.generateTraceId();
  }

  /**
   * 设置当前 trace ID（用于关联同一任务的多条消息）
   */
  setTraceId(traceId: string): void {
    this.traceId = traceId;
  }

  /**
   * 获取当前 trace ID
   */
  getTraceId(): string {
    return this.traceId;
  }

  /**
   * 生成新的 trace ID
   */
  newTrace(): string {
    this.traceId = this.generateTraceId();
    return this.traceId;
  }

  /**
   * 发送进度消息
   * 显示在主对话区，用于展示编排者的进度更新
   */
  progress(phase: string, content: string, options?: { percentage?: number; metadata?: MessageMetadata }): void {
    // 过滤空内容（设计规范：禁止空消息气泡）
    if (!content || !content.trim()) {
      return;
    }

    const message = this.createMessage({
      type: MessageType.PROGRESS,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: {
        phase,
        ...options?.metadata,
      },
    });

    const data: ProgressData & { message: StandardMessage } = {
      phase,
      content,
      percentage: options?.percentage,
      metadata: options?.metadata,
      message,
    };

    this.emit('progress', data);
  }

  /**
   * 发送结果消息
   * 显示在主对话区，用于展示编排者的最终结果
   */
  result(content: string, options?: { success?: boolean; metadata?: MessageMetadata }): void {
    // 过滤空内容
    if (!content || !content.trim()) {
      return;
    }

    const message = this.createMessage({
      type: MessageType.RESULT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: options?.metadata || {},
    });

    const data: ResultData & { message: StandardMessage } = {
      content,
      success: options?.success ?? true,
      metadata: options?.metadata,
      message,
    };

    this.emit('result', data);
  }

  /**
   * 发送 Worker 输出
   * 路由到对应 Worker Tab，不在主对话区显示
   */
  workerOutput(worker: WorkerSlot, content: string, options?: { blocks?: ContentBlock[]; metadata?: MessageMetadata }): void {
    // 过滤空内容
    if (!content || !content.trim()) {
      return;
    }

    const blocks: ContentBlock[] = options?.blocks || [{ type: 'text', content, isMarkdown: true }];

    const message = this.createMessage({
      type: MessageType.TEXT,
      source: 'worker',
      agent: worker as AgentType,
      lifecycle: MessageLifecycle.COMPLETED,
      blocks,
      metadata: options?.metadata || {},
    });

    const data: WorkerOutputData & { message: StandardMessage } = {
      worker,
      content,
      blocks,
      metadata: options?.metadata,
      message,
    };

    this.emit('worker:output', data);
  }

  /**
   * 发送子任务卡片
   * 显示在主对话区，用于展示子任务完成摘要
   */
  subTaskCard(subTask: SubTaskView): void {
    this.emit('subTaskCard', subTask);

    // 同时生成一个编排者消息用于记录
    const statusEmoji = subTask.status === 'completed' ? '✅' : subTask.status === 'failed' ? '❌' : '🔄';
    const content = `${statusEmoji} **${subTask.title}** (${subTask.worker})${subTask.summary ? `\n${subTask.summary}` : ''}`;

    const message = this.createMessage({
      type: MessageType.RESULT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: {
        subTaskId: subTask.id,
        assignedWorker: subTask.worker,
        isStatusMessage: true,
        subTaskCard: subTask,
      },
    });

    this.emit('orchestrator:message', message);
  }

  /**
   * 发送错误消息
   */
  error(error: string, options?: { details?: Record<string, unknown>; recoverable?: boolean }): void {
    // 错误消息不过滤空内容，确保错误被记录
    const errorContent = error || '发生未知错误';

    const message = this.createMessage({
      type: MessageType.ERROR,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.FAILED,
      blocks: [{ type: 'text', content: errorContent }],
      metadata: {
        error: errorContent,
        extra: options?.details,
      },
    });

    const data: ErrorData & { message: StandardMessage } = {
      error: errorContent,
      details: options?.details,
      recoverable: options?.recoverable,
      message,
    };

    this.emit('error', data);
  }

  /**
   * 发送系统通知
   * 显示在主对话区，用于系统级通知
   */
  systemNotice(content: string, metadata?: MessageMetadata): void {
    // 过滤空内容
    if (!content || !content.trim()) {
      return;
    }

    const message = this.createMessage({
      type: MessageType.SYSTEM,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: {
        isStatusMessage: true,
        ...metadata,
      },
    });

    this.emit('system:notice', message);
  }

  /**
   * 发送编排者分析/规划消息
   * 显示在主对话区
   */
  orchestratorMessage(content: string, options?: { type?: MessageType; metadata?: MessageMetadata }): void {
    // 过滤空内容
    if (!content || !content.trim()) {
      return;
    }

    const message = this.createMessage({
      type: options?.type || MessageType.TEXT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: options?.metadata || {},
    });

    this.emit('orchestrator:message', message);
  }

  /**
   * 转发标准消息（LLM/内部流）
   * 用于将 UnifiedMessageBus 的消息统一汇入 MessageHub
   */
  forwardStandardMessage(message: StandardMessage): void {
    this.emit('standard:message', message);
  }

  /**
   * 转发标准流式更新
   */
  forwardStreamUpdate(update: StreamUpdate): void {
    this.emit('standard:update', update);
  }

  /**
   * 转发标准完成消息
   */
  forwardStandardComplete(message: StandardMessage): void {
    this.emit('standard:complete', message);
  }

  /**
   * 创建标准消息
   */
  private createMessage(params: {
    type: MessageType;
    source: MessageSource;
    agent: AgentType;
    lifecycle: MessageLifecycle;
    blocks: ContentBlock[];
    metadata: MessageMetadata;
  }): StandardMessage {
    return createStandardMessage({
      ...params,
      traceId: this.traceId,
    });
  }

  /**
   * 生成 trace ID
   */
  private generateTraceId(): string {
    return `trace_${Date.now()}_${Math.random().toString(36).substring(2, 9)}`;
  }

  /**
   * 销毁 MessageHub
   */
  dispose(): void {
    this.removeAllListeners();
  }
}

/**
 * 全局 MessageHub 实例
 * 用于整个应用的统一消息出口
 */
export const globalMessageHub = new MessageHub();
