/**
 * # MessageHub API 文档
 *
 * MessageHub 是系统的统一消息与事件中心，负责协调编排者(Orchestrator)、工作单元(Worker)和系统(System)之间的消息流转。
 * 它实现了消息的去重、节流和统一路由，确保 UI 呈现的一致性。
 *
 * ## 核心职责
 * 1. **统一出口**: 所有 UI 消息统一通过 MessageHub 发送
 * 2. **主从分离**: 主对话区只承载编排者叙事；Worker 输出在各自 Tab 显示
 * 3. **智能流控**: 内置消息去重（ID/内容）和流式节流（默认 100ms）
 *
 * ## API 概览
 *
 * ### 1. 生命周期管理
 * - `newTrace()`: 生成新 Trace ID，开启新会话
 * - `setTraceId(id)`: 设置当前 Trace ID
 * - `getTraceId()`: 获取当前 Trace ID
 *
 * ### 2. 编排者叙事 (主对话区)
 * - `progress(phase: string, content: string, options?)`: 汇报当前阶段进度
 * - `result(content: string, options?)`: 汇报最终执行结果
 * - `orchestratorMessage(content: string, options?)`: 发送分析/规划类消息
 * - `subTaskCard(subTask: SubTaskView)`: 展示/更新子任务卡片状态
 *
 * ### 3. Worker 交互 (Worker Tab)
 * - `workerOutput(worker: string, content: string, options?)`: 发送 Worker 执行日志
 *
 * ### 4. 系统与错误
 * - `systemNotice(content: string, metadata?)`: 发送系统级通知
 * - `error(err: string, options?)`: 上报错误信息
 *
 * ### 5. 全局通信
 * - `broadcast(msg: string | StandardMessage, options?)`: 向所有组件广播消息
 *
 * ## 典型用法
 *
 * ```typescript
 * const hub = new MessageHub();
 * hub.newTrace(); // 开始新会话
 *
 * // 1. 阶段汇报
 * hub.progress('Planning', '正在制定执行计划...');
 *
 * // 2. 下发任务
 * hub.subTaskCard({
 *   id: 'task-01',
 *   title: '分析依赖',
 *   status: 'running',
 *   worker: 'claude'
 * });
 *
 * // 3. Worker 执行 (独立 Tab)
 * hub.workerOutput('claude', '读取 package.json...');
 *
 * // 4. 任务完成
 * hub.subTaskCard({
 *   id: 'task-01',
 *   title: '分析依赖',
 *   status: 'completed',
 *   worker: 'claude',
 *   summary: '分析完成，发现 3 个问题'
 * });
 *
 * // 5. 最终结果
 * hub.result('依赖分析已完成，准备进行优化。');
 * ```
 *
 * ## 事件订阅
 *
 * ```typescript
 * hub.on('unified:message', (msg) => { ... }); // 监听标准消息
 * hub.on('processingStateChanged', (state) => { ... }); // 监听忙碌状态
 * hub.on('broadcast', (data) => { ... }); // 监听广播消息
 * ```
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import type { WorkerSlot } from '../../types';
import type { StandardMessage, MessageMetadata, ContentBlock, MessageSource, StreamUpdate, NotifyLevel, DataMessageType } from '../../protocol/message-protocol';
import { MessageType, MessageLifecycle, MessageCategory, ControlMessageType, createStandardMessage, createControlMessage, createNotifyMessage, createDataMessage } from '../../protocol/message-protocol';
import { PROCESSING_EVENTS } from '../../protocol/event-names';
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
 * MessageHub 事件类型
 */
export interface MessageHubEvents {
  /** 标准消息（来自 LLM/内部流） */
  'unified:message': (message: StandardMessage) => void;
  /** 标准流式更新 */
  'unified:update': (update: StreamUpdate) => void;
  /** 标准完成消息 */
  'unified:complete': (message: StandardMessage) => void;
  /** 广播消息 */
  'broadcast': (data: { message: StandardMessage; target?: string; timestamp: number }) => void;
  // 🔧 统一消息通道事件
  /** 处理状态变化 */
  'processingStateChanged': (state: ProcessingState) => void;
}

// ============================================================================
// 🔧 统一消息通道类型定义（从 UnifiedMessageBus 迁入）
// ============================================================================

/** MessageHub 配置 */
export interface MessageHubConfig {
  /** 是否启用去重/节流（特性开关） */
  enabled: boolean;
  /** 流式消息最小发送间隔（毫秒） */
  minStreamInterval: number;
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

/** 请求级消息统计 */
interface RequestMessageStats {
  totalContent: number;
  assistantContent: number;
  assistantThreadContent: number;
  assistantWorkerContent: number;
  assistantDispatchContent: number;
  userContent: number;
  placeholderContent: number;
  dataCount: number;
  messageIds: Set<string>;
  /** 创建时间戳（用于自动清理） */
  createdAt: number;
}

/** 请求级消息统计摘要（对外暴露） */
export interface RequestMessageSummary {
  totalContent: number;
  assistantContent: number;
  assistantThreadContent: number;
  assistantWorkerContent: number;
  assistantDispatchContent: number;
  userContent: number;
  placeholderContent: number;
  dataCount: number;
}

/** 内容去重记录 */
interface ContentDedupeRecord {
  messageId: string;
  contentHash: string;
  timestamp: number;
}

/** 处理状态（导出供外部使用） */
export interface ProcessingState {
  isProcessing: boolean;
  source: MessageSource | null;
  agent: string | null;
  startedAt: number | null;
}

/** 默认配置 */
const DEFAULT_HUB_CONFIG: MessageHubConfig = {
  enabled: true,
  minStreamInterval: 100,  // 100ms 节流间隔
  retentionTime: 5 * 60 * 1000,  // 5分钟保留
  debug: false,
};

// ============================================================================
// 内容去重常量
// ============================================================================

const CONTENT_DEDUPE_WINDOW_MS = 30000;  // 30秒窗口

/**
 * MessageHub - 统一消息出口
 *
 * 🔧 统一消息通道核心实现
 *
 * 提供语义化的消息发送 API，所有 UI 消息都通过此类发送：
 * - progress(): 进度消息
 * - result(): 结果消息
 * - workerOutput(): Worker 输出（路由到对应 Tab）
 * - subTaskCard(): 子任务卡片（显示在主对话区）
 * - error(): 错误消息
 *
 * 核心能力（从 UnifiedMessageBus 迁入）：
 * - sendMessage(): 带去重/节流的消息发送
 * - sendUpdate(): 流式更新发送
 * - getProcessingState(): 获取处理状态
 */
export class MessageHub extends EventEmitter {
  private traceId: string;
  private requestId?: string;

  // ==========================================================================
  // 🔧 统一消息通道状态（从 UnifiedMessageBus 迁入）
  // ==========================================================================
  private config: MessageHubConfig;
  private messageStates: Map<string, MessageState> = new Map();
  private processingState: ProcessingState = {
    isProcessing: false,
    source: null,
    agent: null,
    startedAt: null,
  };
  // 🔧 根治：移除 activeMessageIds Set，消除双数据源同步问题
  // 活动消息状态完全由 messageStates.completed 派生
  private cleanupTimer: NodeJS.Timeout | null = null;
  private contentDedupeRecords: ContentDedupeRecord[] = [];
  /** 防止事件发射触发的重入调用 */
  private processingMessageIds: Set<string> = new Set();
  private requestMessageStats: Map<string, RequestMessageStats> = new Map();
  private requestMessageIdMap: Map<string, string> = new Map();
  private streamBuffers: Map<string, { text: string; lastBlocks?: ContentBlock[] }> = new Map();

  // ==========================================================================
  // 🔧 状态一致性：统一使用 messageStates 管理活动消息
  // 根治：消除 activeMessageIds 双数据源，单一数据源保证一致性
  // ==========================================================================

  /**
   * 添加活动消息
   */
  private addActiveMessage(id: string, message: StandardMessage, timestamp: number): void {
    this.messageStates.set(id, {
      message,
      lastSentAt: timestamp,
      lastStreamAt: timestamp,
      completed: false,
    });
  }

  /**
   * 标记消息完成
   */
  private markMessageComplete(id: string, message: StandardMessage, timestamp: number): void {
    const state = this.messageStates.get(id);
    if (state) {
      state.message = message;
      state.lastSentAt = timestamp;
      state.completed = true;
    }
    this.streamBuffers.delete(id);
  }

  /**
   * 检查是否有活动消息
   * 🔧 根治：直接从 messageStates 派生，无需兜底逻辑
   */
  private hasActiveMessages(): boolean {
    for (const state of this.messageStates.values()) {
      if (!state.completed) {
        return true;
      }
    }
    return false;
  }

  constructor(traceId?: string, config?: Partial<MessageHubConfig>) {
    super();
    this.traceId = traceId || this.generateTraceId();
    this.config = { ...DEFAULT_HUB_CONFIG, ...config };
    this.startCleanupTimer();
  }

  /**
   * 设置当前 trace ID（用于关联同一任务的多条消息）
   */
  setTraceId(traceId: string): void {
    this.traceId = traceId;
  }

  setRequestContext(requestId?: string): void {
    this.requestId = requestId;
  }

  getRequestContext(): string | undefined {
    return this.requestId;
  }

  getRequestMessageStats(requestId: string): RequestMessageSummary | undefined {
    const stats = this.requestMessageStats.get(requestId);
    if (!stats) {
      return undefined;
    }
    return this.toRequestSummary(stats);
  }

  finalizeRequestContext(requestId: string): RequestMessageSummary | undefined {
    const stats = this.requestMessageStats.get(requestId);
    if (!stats) {
      return undefined;
    }
    this.requestMessageStats.delete(requestId);
    this.requestMessageIdMap.delete(requestId);
    return this.toRequestSummary(stats);
  }

  getRequestMessageId(requestId: string): string | undefined {
    return this.requestMessageIdMap.get(requestId);
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

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
  }

  /**
   * 发送结果消息
   * 显示在主对话区，用于展示编排者的最终结果
   */
  result(content: string, options?: { success?: boolean; metadata?: MessageMetadata }): void {
    // 过滤空内容
    if (!content || !content.trim()) {
      logger.warn('MessageHub.result.空内容跳过', undefined, LogCategory.SYSTEM);
      return;
    }

    const requestId = (options?.metadata as { requestId?: string } | undefined)?.requestId || this.requestId;
    const reuseMessageId = requestId ? this.getRequestMessageId(requestId) : undefined;
    const message = createStandardMessage({
      id: reuseMessageId,
      traceId: this.traceId,
      category: MessageCategory.CONTENT,
      type: MessageType.RESULT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: options?.metadata || {},
    });

    logger.info('MessageHub.result.发送', {
      id: message.id,
      category: message.category,
      contentLength: content.length,
      contentPreview: content.substring(0, 100),
      metadata: options?.metadata,
    }, LogCategory.SYSTEM);

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
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

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
  }

  /**
   * 发送子任务卡片
   * 显示在主对话区，用于展示子任务完成摘要
   *
   * 使用自然语言格式，不再使用固定模板
   */
  subTaskCard(subTask: SubTaskView): void {
    // 生成自然语言的状态描述
    let content: string;
    const workerName = subTask.worker;

    if (subTask.status === 'completed') {
      // 完成状态：使用摘要或标题
      if (subTask.summary) {
        content = `${workerName} 已完成：${subTask.summary}`;
      } else {
        content = `${workerName} 完成了任务`;
      }
    } else if (subTask.status === 'failed') {
      // 失败状态：显示错误信息
      content = `${workerName} 执行遇到问题：${subTask.summary || '执行失败'}`;
    } else {
      // 进行中状态：显示任务标题
      content = `${workerName} 正在处理：${subTask.title}`;
    }

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

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
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
        recoverable: options?.recoverable,
      },
    });

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
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

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
  }

  /**
   * 发送编排者分析/规划消息
   * 显示在主对话区
   */
  orchestratorMessage(content: string, options?: { type?: MessageType; metadata?: MessageMetadata }): void {
    // 过滤空内容
    if (!content || !content.trim()) {
      logger.warn('MessageHub.orchestratorMessage.空内容跳过', undefined, LogCategory.SYSTEM);
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

    logger.info('MessageHub.orchestratorMessage.发送', {
      id: message.id,
      category: message.category,
      contentLength: content.length,
      contentPreview: content.substring(0, 100),
    }, LogCategory.SYSTEM);

    // 🔧 统一出口：所有消息通过 sendMessage 进入统一通道
    this.sendMessage(message);
  }

  /**
   * 广播消息给所有订阅者
   * 用于跨组件通信和全局事件通知
   */
  broadcast(message: string | StandardMessage, options?: { target?: string; metadata?: MessageMetadata }): void {
    // 1. 如果是简单字符串，包装成标准消息
    const standardMessage = typeof message === 'string'
      ? this.createMessage({
          type: MessageType.TEXT, // 默认为文本消息
          source: 'orchestrator',       // 使用 orchestrator 作为来源
          agent: 'orchestrator',
          lifecycle: MessageLifecycle.COMPLETED,
          blocks: [{ type: 'text', content: message }],
          metadata: options?.metadata || {},
        })
      : message;

    // 2. 记录并处理消息
    // 使用 sendMessage 进行去重和流控处理
    this.sendMessage(standardMessage);

    // 3. 触发广播事件 (专门的广播通道)
    // 订阅者可以通过监听 'broadcast' 事件来接收所有广播消息
    this.safeEmit('broadcast', {
      message: standardMessage,
      target: options?.target,
      timestamp: Date.now()
    });
    
    // 4. 系统类消息无需额外事件，已通过 sendMessage 统一通道处理
  }

  // ==========================================================================
  // 🔧 统一消息通道核心 API（从 UnifiedMessageBus 迁入）
  // ==========================================================================

  /**
   * 发送标准消息（带去重/节流）
   *
   * 🔧 核心消息发送入口，替代 UnifiedMessageBus.sendMessage
   *
   * 去重逻辑：
   * - ID 去重：相同 ID 的 STARTED 消息只发送一次
   * - 内容去重：不同 ID 但内容相同的消息（30秒窗口内）
   *
   * 节流逻辑：
   * - STREAMING 消息间隔不低于 minStreamInterval (100ms)
   */
  sendMessage(message: StandardMessage): boolean {
    // 🔧 调试日志：追踪所有进入 sendMessage 的消息
    console.log('[MessageHub] sendMessage 入口:', {
      id: message.id,
      category: message.category,
      type: message.type,
      lifecycle: message.lifecycle,
      source: message.source,
      agent: message.agent,
      blocksCount: message.blocks?.length,
      hasRequestId: !!message.metadata?.requestId,
      currentRequestContext: this.requestId,
    });

    // 🔧 重入保护：防止事件监听器触发的递归调用
    if (message.id && this.processingMessageIds.has(message.id)) {
      this.debugLog('跳过消息 [RE-ENTRANT]', message.id);
      return false;
    }

    if (message.id) {
      this.processingMessageIds.add(message.id);
    }

    try {
      return this.doSendMessage(message);
    } finally {
      if (message.id) {
        this.processingMessageIds.delete(message.id);
      }
    }
  }

  /**
   * 实际的消息发送逻辑（内部方法）
   */
  private doSendMessage(message: StandardMessage): boolean {
    if (this.requestId && !message.metadata?.requestId) {
      message = {
        ...message,
        metadata: {
          ...(message.metadata || {}),
          requestId: this.requestId,
        },
      };
    }

    const requestId = message.metadata?.requestId;
    if (requestId && message.metadata?.isPlaceholder === true) {
      this.requestMessageIdMap.set(requestId, message.id);
      console.log('[MessageHub] 注册占位消息 ID 映射:', {
        requestId,
        placeholderMessageId: message.id,
        mapSize: this.requestMessageIdMap.size,
      });
    }

    const isPlaceholder = message.metadata?.isPlaceholder === true;
    const isUser = message.metadata?.role === 'user';
    const isStatusMessageMeta = message.metadata?.isStatusMessage === true;
    const hasSubTaskCard = Boolean(message.metadata?.subTaskCard);
    const dispatchToWorker = message.metadata?.dispatchToWorker === true;
    if (
      message.category === MessageCategory.CONTENT
      && message.source === 'orchestrator'
      && requestId
      && !isPlaceholder
      && !isUser
      && !isStatusMessageMeta
      && !hasSubTaskCard
      && !dispatchToWorker
    ) {
      const boundMessageId = this.getRequestMessageId(requestId);
      if (!boundMessageId) {
        throw new Error(`[MessageHub] 主响应消息缺少占位绑定: requestId=${requestId}`);
      }
      if (message.id !== boundMessageId) {
        message = { ...message, id: boundMessageId };
      }
    }

    if (message.category === MessageCategory.CONTENT) {
      if (typeof requestId !== 'string' || !requestId.trim()) {
        // 🔧 改为警告而非抛出异常，避免阻塞消息流
        // requestId 缺失时记录警告，但仍然发送消息
        logger.warn('MessageHub.内容消息缺少requestId', {
          id: message.id,
          source: message.source,
          agent: message.agent,
          lifecycle: message.lifecycle,
        }, LogCategory.SYSTEM);
      }
    }
    if (!message.id || !message.id.trim()) {
      throw new Error('[MessageHub] StandardMessage missing id');
    }
    if (Array.isArray(message.blocks)) {
      const invalidBlocks = message.blocks.filter(
        (block) => !block || typeof block !== 'object' || typeof (block as any).type !== 'string'
      );
      if (invalidBlocks.length > 0) {
        logger.error('MessageHub.块_无效', {
          id: message.id,
          invalidCount: invalidBlocks.length,
        }, LogCategory.SYSTEM);
        throw new Error(`[MessageHub] Invalid content blocks: ${message.id}`);
      }
    }

    // 必填字段检查
    if (!message.source || !message.agent) {
      logger.error('MessageHub.消息字段缺失', {
        id: message.id,
        source: message.source,
        agent: message.agent,
        type: message.type,
        lifecycle: message.lifecycle
      }, LogCategory.SYSTEM);
      throw new Error(`[MessageHub] StandardMessage missing source/agent: ${message.id}`);
    }

    // 🔧 严格校验消息类别与专属字段，禁止 data-only 误用
    if (!message.category) {
      throw new Error(`[MessageHub] StandardMessage missing category: ${message.id}`);
    }

    // 🔧 修复：对于 CONTENT 消息，先从流缓冲区填充 blocks，再进行验证
    // 避免 COMPLETED 消息因 blocks 为空而被误拦截
    if (message.category === MessageCategory.CONTENT) {
      this.updateStreamBufferFromMessage(message);
      message = this.ensureContentBlocksFromBuffer(message);
    }

    switch (message.category) {
      case MessageCategory.CONTENT: {
        const isPlaceholder = message.metadata?.isPlaceholder === true;
        const isStreaming = message.lifecycle === MessageLifecycle.STARTED || message.lifecycle === MessageLifecycle.STREAMING;
        const isUser = message.metadata?.role === 'user';
        const hasBlocks = Array.isArray(message.blocks) && message.blocks.length > 0;
        // 🔧 根治：禁止 data-only 内容流。非占位、非流式、非用户消息必须有内容块。
        if (!hasBlocks && !isPlaceholder && !isStreaming && !isUser) {
          throw new Error(`[MessageHub] Content message missing blocks: ${message.id}`);
        }
        break;
      }
      case MessageCategory.CONTROL:
        if (!message.control) {
          throw new Error(`[MessageHub] Control message missing control payload: ${message.id}`);
        }
        break;
      case MessageCategory.NOTIFY:
        if (!message.notify) {
          throw new Error(`[MessageHub] Notify message missing notify payload: ${message.id}`);
        }
        break;
      case MessageCategory.DATA:
        if (!message.data) {
          throw new Error(`[MessageHub] Data message missing data payload: ${message.id}`);
        }
        if (Array.isArray(message.blocks) && message.blocks.length > 0) {
          throw new Error(`[MessageHub] Data message must not carry blocks: ${message.id}`);
        }
        break;
      default:
        throw new Error(`[MessageHub] Unknown message category: ${String(message.category)} (${message.id})`);
    }

    // 如果禁用去重/节流，直接发送
    if (!this.config.enabled) {
      this.recordRequestMessage(message);
      this.emitByCategory(message);
      return true;
    }

    const { id, lifecycle } = message;
    const now = Date.now();
    const existingState = this.messageStates.get(id);

    // 1. 重复 STARTED 消息：拒绝
    if (existingState && lifecycle === MessageLifecycle.STARTED) {
      logger.warn('MessageHub.重复_START', {
        id,
        source: message.source,
        agent: message.agent,
        lifecycle,
      }, LogCategory.SYSTEM);
      return false;
    }

    // 2. 内容去重（非流式、非状态消息）
    const isStatusMessage = message.metadata?.isStatusMessage === true;
    const isProgressMessage = message.type === 'progress';
    const shouldDedupe = isStatusMessage || isProgressMessage;
    if (shouldDedupe && lifecycle !== MessageLifecycle.STARTED && lifecycle !== MessageLifecycle.STREAMING) {
      const contentHash = this.computeContentHash(message);
      if (contentHash && this.isDuplicateContent(contentHash, now)) {
        this.debugLog('跳过消息 [CONTENT_DUPLICATE]', id);
        return false;
      }
      if (contentHash) {
        this.recordContentHash(id, contentHash, now);
      }
    }

    // 3. STARTED 消息：总是发送，激活 processingState
    if (lifecycle === MessageLifecycle.STARTED) {
      console.log('[MessageHub] 发送 STARTED 消息:', {
        id,
        source: message.source,
        agent: message.agent,
        isPlaceholder: message.metadata?.isPlaceholder,
      });
      this.recordMessage(message, now);
      this.updateProcessingState(true, message.source, message.agent);
      this.recordRequestMessage(message);
      this.emitByCategory(message);
      this.debugLog('发送消息 [STARTED]', id);
      return true;
    }

    // 4. 新消息：发送
    if (!existingState) {
      console.log('[MessageHub] 发送 NEW 消息:', {
        id,
        lifecycle,
        source: message.source,
        agent: message.agent,
        category: message.category,
        blocksCount: message.blocks?.length ?? 0,
      });
      this.recordMessage(message, now);
      if (lifecycle === MessageLifecycle.STREAMING) {
        this.updateProcessingState(true, message.source, message.agent);
      }
      this.recordRequestMessage(message);
      this.emitByCategory(message);
      if (this.isTerminalLifecycle(lifecycle)) {
        this.completeMessage(id, message, now);
        this.safeEmit('unified:complete', message);
        this.checkAndUpdateProcessingState();
      }
      this.debugLog('发送消息 [NEW]', id);
      return true;
    }

    // 5. 已完成的消息：不再发送
    if (existingState.completed) {
      logger.warn('MessageHub.重复_完成', {
        id,
        source: message.source,
        agent: message.agent,
        lifecycle,
      }, LogCategory.SYSTEM);
      this.debugLog('跳过消息 [COMPLETED]', id);
      return false;
    }

    // 6. STREAMING 消息：检查节流间隔
    if (lifecycle === MessageLifecycle.STREAMING) {
      const timeSinceLastStream = now - existingState.lastStreamAt;
      if (timeSinceLastStream < this.config.minStreamInterval) {
        this.debugLog('跳过消息 [THROTTLE]', id);
        return false;
      }
      existingState.lastStreamAt = now;
      existingState.message = message;
      this.recordRequestMessage(message);
      this.emitByCategory(message);
      this.debugLog('发送消息 [STREAMING]', id);
      return true;
    }

    // 7. 终态消息（COMPLETED/FAILED/CANCELLED）：标记完成，发送
    if (this.isTerminalLifecycle(lifecycle)) {
      this.completeMessage(id, message, now);
      this.recordRequestMessage(message);
      this.safeEmit('unified:complete', message);
      this.checkAndUpdateProcessingState();
      this.debugLog('发送消息 [COMPLETE]', id);
      return true;
    }

    return true;
  }

  /**
   * 安全的事件发射方法
   *
   * 🔧 设计决策：
   * - 记录监听器异常并继续执行，确保消息流不会因单个监听器错误而中断
   * - 始终记录错误到日志，便于问题排查
   * - 在 debug 模式下会重新抛出异常，帮助开发者定位问题
   */
  private safeEmit(event: string, data: unknown): boolean {
    try {
      return this.emit(event, data);
    } catch (error) {
      logger.error('MessageHub.event_emit_failed', {
        event,
        error: error instanceof Error ? error.message : String(error),
        stack: error instanceof Error ? error.stack : undefined,
        messageId: (data as StandardMessage)?.id,
      }, LogCategory.SYSTEM);
      // 🔧 debug 模式下重新抛出，帮助开发者定位问题
      if (this.config.debug) {
        throw error;
      }
      return false;
    }
  }

  /**
   * 根据消息分类发送到对应事件通道
   *
   * 🔧 统一消息通道：统一走 unified:message
   */
  private emitByCategory(message: StandardMessage): void {
    this.safeEmit('unified:message', message);
  }

  /**
   * 发送流式更新（带节流）
   *
   * 🔧 核心流式更新入口，替代 UnifiedMessageBus.sendUpdate
   */
  sendUpdate(update: StreamUpdate): boolean {
    if (!update.messageId || !update.messageId.trim()) {
      throw new Error('[MessageHub] StreamUpdate missing messageId');
    }
    if (Array.isArray(update.blocks)) {
      const invalidBlocks = update.blocks.filter(
        (block) => !block || typeof block !== 'object' || typeof (block as any).type !== 'string'
      );
      if (invalidBlocks.length > 0) {
        logger.error('MessageHub.更新_块无效', {
          messageId: update.messageId,
          invalidCount: invalidBlocks.length,
        }, LogCategory.SYSTEM);
        throw new Error(`[MessageHub] Invalid update blocks: ${update.messageId}`);
      }
    }

    // 如果禁用节流，直接发送
    if (!this.config.enabled) {
      this.updateStreamBufferFromUpdate(update);
      this.safeEmit('unified:update', update);
      return true;
    }

    const now = Date.now();
    this.updateStreamBufferFromUpdate(update);
    const state = this.messageStates.get(update.messageId);

    // 无状态：拒绝
    if (!state) {
      logger.warn('MessageHub.更新缺少状态', { messageId: update.messageId }, LogCategory.SYSTEM);
      this.debugLog('跳过更新 [NO_STATE]', update.messageId);
      return false;
    }

    // 已完成：拒绝
    if (state.completed) {
      logger.warn('MessageHub.完成后更新', { messageId: update.messageId }, LogCategory.SYSTEM);
      this.debugLog('跳过更新 [COMPLETED]', update.messageId);
      return false;
    }

    // 节流检查
    const timeSinceLastStream = now - state.lastStreamAt;
    if (timeSinceLastStream < this.config.minStreamInterval) {
      this.debugLog('跳过更新 [THROTTLE]', update.messageId);
      return false;
    }

    state.lastStreamAt = now;
    this.safeEmit('unified:update', update);
    this.debugLog('发送更新', update.messageId);
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
      // 🔧 根治：直接遍历 messageStates 标记完成，无需额外数据结构
      const now = Date.now();
      for (const state of this.messageStates.values()) {
        if (!state.completed) {
          state.completed = true;
          state.lastSentAt = now;
        }
      }
      this.processingMessageIds.clear();
    }
    this.updateProcessingState(isProcessing, null, null);
  }

  // ==========================================================================
  // 🔧 统一消息通道控制 API（Task 1.4）
  // ==========================================================================

  /**
   * 发送控制消息
   *
   * @param controlType 控制消息类型
   * @param payload 控制消息负载
   */
  sendControl(controlType: ControlMessageType, payload: Record<string, unknown>): void {
    const message = createControlMessage(controlType, payload, this.traceId);
    this.sendMessage(message);
  }

  /**
   * 发送通知消息（NOTIFY）
   */
  notify(content: string, level: NotifyLevel = 'info', duration?: number): void {
    if (!content || !content.trim()) {
      return;
    }
    const message = createNotifyMessage(content, level, this.traceId, duration);
    this.sendMessage(message);
  }

  /**
   * 发送数据同步消息（DATA）
   */
  data(dataType: DataMessageType, payload: Record<string, unknown>): void {
    const message = createDataMessage(dataType, payload, this.traceId);
    this.sendMessage(message);
  }

  /**
   * 阶段变化通知
   *
   * @param phase 阶段名称
   * @param isRunning 是否正在运行
   * @param taskId 可选任务 ID
   */
  phaseChange(phase: string, isRunning: boolean, taskId?: string): void {
    this.sendControl(ControlMessageType.PHASE_CHANGED, {
      phase,
      isRunning,
      taskId,
      timestamp: Date.now(),
    });
  }

  /**
   * 任务已接受确认
   *
   * @param requestId 请求 ID
   */
  taskAccepted(requestId: string): void {
    this.sendControl(ControlMessageType.TASK_ACCEPTED, {
      requestId,
      timestamp: Date.now(),
    });
  }

  /**
   * 任务被拒绝通知
   *
   * @param requestId 请求 ID
   * @param reason 拒绝原因
   */
  taskRejected(requestId: string, reason: string): void {
    this.sendControl(ControlMessageType.TASK_REJECTED, {
      requestId,
      reason,
      timestamp: Date.now(),
    });
  }

  /**
   * Worker 状态更新
   *
   * @param worker Worker 名称
   * @param available 是否可用
   */
  workerStatus(worker: string, available: boolean, model?: string): void {
    this.sendControl(ControlMessageType.WORKER_STATUS, {
      worker,
      available,
      model,
      timestamp: Date.now(),
    });
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
    category?: MessageCategory;
  }): StandardMessage {
    return createStandardMessage({
      ...params,
      traceId: this.traceId,
      category: params.category || MessageCategory.CONTENT,  // 🔧 默认 CONTENT 类别
    });
  }

  /**
   * 生成 trace ID
   */
  private generateTraceId(): string {
    return `trace_${Date.now()}_${Math.random().toString(36).substring(2, 9)}`;
  }

  // ==========================================================================
  // 🔧 统一消息通道私有方法（从 UnifiedMessageBus 迁入）
  // ==========================================================================

  /**
   * 记录消息状态（委托给统一方法）
   */
  private recordMessage(message: StandardMessage, timestamp: number): void {
    this.addActiveMessage(message.id, message, timestamp);
  }

  private recordRequestMessage(message: StandardMessage): void {
    const requestId = message.metadata?.requestId;
    if (!requestId) {
      return;
    }
    const now = Date.now();
    const stats = this.requestMessageStats.get(requestId) || {
      totalContent: 0,
      assistantContent: 0,
      assistantThreadContent: 0,
      assistantWorkerContent: 0,
      assistantDispatchContent: 0,
      userContent: 0,
      placeholderContent: 0,
      dataCount: 0,
      messageIds: new Set<string>(),
      createdAt: now,
    };
    if (stats.messageIds.has(message.id)) {
      return;
    }
    stats.messageIds.add(message.id);

    if (message.category === MessageCategory.DATA) {
      stats.dataCount += 1;
      this.requestMessageStats.set(requestId, stats);
      return;
    }

    if (message.category !== MessageCategory.CONTENT) {
      this.requestMessageStats.set(requestId, stats);
      return;
    }

    const isStatusMessage = message.metadata?.isStatusMessage === true;
    const isProgressMessage = message.type === MessageType.PROGRESS;
    if (isStatusMessage || isProgressMessage) {
      this.requestMessageStats.set(requestId, stats);
      return;
    }

    const hasText = Boolean(this.extractTextFromBlocks(message.blocks))
      || Boolean(this.streamBuffers.get(message.id)?.text);
    const hasBlocks = this.hasRenderableBlocks(message.blocks)
      || this.hasRenderableBlocks(this.streamBuffers.get(message.id)?.lastBlocks);
    const isPlaceholder = message.metadata?.isPlaceholder === true;
    const isUser = message.metadata?.role === 'user';
    const isDispatchToWorker = message.metadata?.dispatchToWorker === true;
    const isWorkerSource = message.source === 'worker';
    const isOrchestratorSource = message.source === 'orchestrator';

    if (!hasText && !hasBlocks && !isPlaceholder && !isUser) {
      this.requestMessageStats.set(requestId, stats);
      return;
    }

    stats.totalContent += 1;
    if (isPlaceholder) {
      stats.placeholderContent += 1;
    } else if (isUser) {
      stats.userContent += 1;
    } else if (isWorkerSource) {
      stats.assistantWorkerContent += 1;
      stats.assistantContent += 1;
    } else if (isDispatchToWorker) {
      stats.assistantDispatchContent += 1;
    } else if (isOrchestratorSource) {
      stats.assistantThreadContent += 1;
      stats.assistantContent += 1;
    } else {
      stats.assistantContent += 1;
    }
    this.requestMessageStats.set(requestId, stats);
  }

  private toRequestSummary(stats: RequestMessageStats): RequestMessageSummary {
    return {
      totalContent: stats.totalContent,
      assistantContent: stats.assistantContent,
      assistantThreadContent: stats.assistantThreadContent,
      assistantWorkerContent: stats.assistantWorkerContent,
      assistantDispatchContent: stats.assistantDispatchContent,
      userContent: stats.userContent,
      placeholderContent: stats.placeholderContent,
      dataCount: stats.dataCount,
    };
  }

  /**
   * 标记消息完成（委托给统一方法）
   */
  private completeMessage(id: string, message: StandardMessage, timestamp: number): void {
    this.markMessageComplete(id, message, timestamp);
  }

  /**
   * 判断是否为终态生命周期
   */
  private isTerminalLifecycle(lifecycle: MessageLifecycle): boolean {
    return (
      lifecycle === MessageLifecycle.COMPLETED ||
      lifecycle === MessageLifecycle.FAILED ||
      lifecycle === MessageLifecycle.CANCELLED
    );
  }

  /**
   * 更新处理状态
   */
  private updateProcessingState(isProcessing: boolean, source: MessageSource | null, agent: string | null): void {
    const prev = this.processingState.isProcessing;
    this.processingState = {
      isProcessing,
      source: isProcessing ? source : null,
      agent: isProcessing ? agent : null,
      startedAt: isProcessing ? (this.processingState.startedAt || Date.now()) : null,
    };
    if (prev !== isProcessing) {
      this.safeEmit(PROCESSING_EVENTS.STATE_CHANGED, this.getProcessingState());
    }
  }

  /**
   * 检查并更新处理状态
   */
  private checkAndUpdateProcessingState(): void {
    if (!this.hasActiveMessages()) {
      this.updateProcessingState(false, null, null);
    }
  }

  /**
   * 启动清理定时器
   */
  private startCleanupTimer(): void {
    this.cleanupTimer = setInterval(() => {
      try {
        const now = Date.now();
        const expireTime = now - this.config.retentionTime;

        // 🔧 修复：先收集要删除的 ID，避免迭代中删除
        const messageIdsToDelete: string[] = [];
        for (const [id, state] of this.messageStates) {
          if (state.completed && state.lastSentAt < expireTime) {
            messageIdsToDelete.push(id);
          }
        }
        for (const id of messageIdsToDelete) {
          this.messageStates.delete(id);
        }

        // 🔧 清理孤立的 streamBuffers（无对应 state 或已过期）
        const bufferIdsToDelete: string[] = [];
        for (const [id] of this.streamBuffers) {
          const state = this.messageStates.get(id);
          if (!state || state.completed) {
            bufferIdsToDelete.push(id);
          }
        }
        for (const id of bufferIdsToDelete) {
          this.streamBuffers.delete(id);
        }

        // 🔧 根治：清理过期的 requestMessageStats（超过保留时间）
        const requestIdsToDelete: string[] = [];
        for (const [requestId, stats] of this.requestMessageStats) {
          if (stats.createdAt < expireTime) {
            requestIdsToDelete.push(requestId);
          }
        }
        for (const requestId of requestIdsToDelete) {
          this.requestMessageStats.delete(requestId);
        }

        // 清理过期的内容去重记录
        this.cleanupContentDedupeRecords(now);
      } catch (error) {
        logger.error('MessageHub.cleanup_timer_failed', {
          error: error instanceof Error ? error.message : String(error),
          messageStatesSize: this.messageStates.size,
          streamBuffersSize: this.streamBuffers.size,
        }, LogCategory.SYSTEM);
      }
    }, 60 * 1000);
  }

  /**
   * 调试日志
   */
  private debugLog(action: string, messageId: string): void {
    if (this.config.debug) {
      logger.debug('MessageHub.' + action, { messageId }, LogCategory.SYSTEM);
    }
  }

  // ==========================================================================
  // 🔧 内容去重辅助方法（从 UnifiedMessageBus 迁入）
  // ==========================================================================

  /**
   * 计算消息内容的哈希值
   */
  private computeContentHash(message: StandardMessage): string | null {
    const textParts: string[] = [];
    for (const block of message.blocks || []) {
      if (block.type === 'text' && block.content) {
        textParts.push(block.content);
      }
    }

    if (textParts.length === 0) {
      return null;  // 无文本内容，不参与去重
    }

    // 规范化内容
    const normalized = textParts
      .join(' ')
      .replace(/#{1,6}\s*/g, '')  // 移除 markdown 标题符号
      .replace(/\s+/g, ' ')
      .trim()
      .toLowerCase();

    if (normalized.length < 10) {
      return null;  // 内容太短，不参与去重
    }

    // 简单哈希：取前100字符 + 长度 + 来源
    return `${message.source}:${normalized.slice(0, 100)}:${normalized.length}`;
  }

  /**
   * 检查是否存在重复内容
   */
  private isDuplicateContent(contentHash: string, now: number): boolean {
    const windowStart = now - CONTENT_DEDUPE_WINDOW_MS;

    for (const record of this.contentDedupeRecords) {
      if (record.timestamp >= windowStart && record.contentHash === contentHash) {
        return true;
      }
    }

    return false;
  }

  /**
   * 记录内容哈希
   */
  private recordContentHash(messageId: string, contentHash: string, timestamp: number): void {
    this.contentDedupeRecords.push({
      messageId,
      contentHash,
      timestamp,
    });

    // 限制记录数量
    if (this.contentDedupeRecords.length > 100) {
      this.contentDedupeRecords = this.contentDedupeRecords.slice(-50);
    }
  }

  /**
   * 清理过期的内容去重记录
   */
  private cleanupContentDedupeRecords(now: number): void {
    const windowStart = now - CONTENT_DEDUPE_WINDOW_MS;
    this.contentDedupeRecords = this.contentDedupeRecords.filter(
      record => record.timestamp >= windowStart
    );
  }

  private extractTextFromBlocks(blocks?: ContentBlock[]): string {
    if (!Array.isArray(blocks) || blocks.length === 0) {
      return '';
    }
    return blocks
      .filter((block) => block?.type === 'text' || block?.type === 'thinking')
      .map((block) => (block as any).content || '')
      .filter(Boolean)
      .join('\n');
  }

  private hasRenderableBlocks(blocks?: ContentBlock[]): boolean {
    if (!Array.isArray(blocks) || blocks.length === 0) {
      return false;
    }
    return blocks.some((block) => {
      if (!block || typeof block !== 'object') return false;
      switch (block.type) {
        case 'text':
        case 'thinking':
          return Boolean((block as any).content && String((block as any).content).trim());
        case 'tool_call':
        case 'file_change':
        case 'plan':
          return true;
        default:
          return Boolean((block as any).content && String((block as any).content).trim());
      }
    });
  }

  private updateStreamBufferFromMessage(message: StandardMessage): void {
    if (message.category !== MessageCategory.CONTENT) {
      return;
    }
    const buffer = this.streamBuffers.get(message.id) || { text: '' };
    if (Array.isArray(message.blocks) && message.blocks.length > 0) {
      buffer.lastBlocks = message.blocks;
    }
    const text = this.extractTextFromBlocks(message.blocks);
    if (text) {
      buffer.text = text;
    }
    this.streamBuffers.set(message.id, buffer);
  }

  private updateStreamBufferFromUpdate(update: StreamUpdate): void {
    const buffer = this.streamBuffers.get(update.messageId) || { text: '' };
    if (update.updateType === 'append' && update.appendText) {
      buffer.text = `${buffer.text}${update.appendText}`;
    } else if ((update.updateType === 'replace' || update.updateType === 'block_update') && update.blocks) {
      buffer.text = this.extractTextFromBlocks(update.blocks);
      buffer.lastBlocks = update.blocks;
    }
    this.streamBuffers.set(update.messageId, buffer);
  }

  private ensureContentBlocksFromBuffer(message: StandardMessage): StandardMessage {
    if (message.category !== MessageCategory.CONTENT) {
      return message;
    }
    if (message.metadata?.isPlaceholder) {
      return message;
    }
    const hasBlocks = Array.isArray(message.blocks) && message.blocks.length > 0;
    const existingText = this.extractTextFromBlocks(message.blocks);
    if (hasBlocks && existingText && existingText.trim()) {
      return message;
    }
    const buffer = this.streamBuffers.get(message.id);
    if (buffer?.lastBlocks && buffer.lastBlocks.length > 0) {
      return {
        ...message,
        blocks: buffer.lastBlocks,
        updatedAt: Date.now(),
      };
    }
    if (!buffer?.text || !buffer.text.trim()) {
      return message;
    }
    return {
      ...message,
      blocks: [{ type: 'text', content: buffer.text, isMarkdown: true }],
      updatedAt: Date.now(),
    };
  }

  /**
   * 销毁 MessageHub
   */
  dispose(): void {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
    this.messageStates.clear();
    this.processingMessageIds.clear();
    this.contentDedupeRecords = [];
    this.requestMessageStats.clear();
    this.streamBuffers.clear();
    // 🔧 重置状态，确保完全清理
    this.processingState = {
      isProcessing: false,
      source: null,
      agent: null,
      startedAt: null,
    };
    this.requestId = undefined;
    this.removeAllListeners();
  }
}

/**
 * 全局 MessageHub 实例
 * 用于整个应用的统一消息出口
 */
export const globalMessageHub = new MessageHub();
