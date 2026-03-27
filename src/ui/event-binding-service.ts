/**
 * EventBindingService - 事件绑定服务
 *
 * 从 WebviewProvider 提取的独立模块（#18 WVP 瘦身）。
 * 职责：
 * - globalEventBus 事件监听 → UI 状态同步
 * - MessageHub 消息订阅 → Webview 转发
 * - Adapter 错误事件监听
 * - MissionOrchestrator 事件监听 → Mission/Todo 状态转发
 * - 工具授权状态管理（请求队列 + 超时）
 */

import { logger, LogCategory } from '../logging';
import { t } from '../i18n';
import type {
  WorkerSlot,
  ExtensionToWebviewMessage,
  LogEntry,
} from '../types';
import {
  StandardMessage,
  StreamUpdate,
  DataMessageType,
  NotifyLevel,
  InteractionType,
  MessageCategory,
  MessageLifecycle,
  MessageType,
  createInteractionMessage,
} from '../protocol/message-protocol';
import { ADAPTER_EVENTS, PROCESSING_EVENTS, WEBVIEW_MESSAGE_TYPES } from '../protocol/event-names';
import { globalEventBus } from '../events';
import type { IAdapterFactory } from '../adapters/adapter-factory-interface';
import type { MissionDrivenEngine } from '../orchestrator/core';
import type { MessageHub } from '../orchestrator/core/message-hub';
import type { MissionOrchestrator } from '../orchestrator/core';
import { normalizeTodos, generateEntityId } from '../orchestrator/mission/data-normalizer';
import { isModelOriginIssue, toModelOriginUserMessage } from '../errors/model-origin';
import { resolveStandardMessageSessionBinding } from '../session/standard-message-session-binding';
import type { SessionTimelineProjection } from '../session/session-timeline-projection';
import { messageHasRenderableTimelineContent } from '../shared/timeline-presentation';

// ============================================================================
// 上下文接口 - EventBindingService 对 WVP 的依赖声明
// ============================================================================

export interface EventBindingContext {
  // 状态访问器
  getActiveSessionId(): string | null;
  getMessageHub(): MessageHub;
  getOrchestratorEngine(): MissionDrivenEngine;
  getAdapterFactory(): IAdapterFactory;
  getMissionOrchestrator(): MissionOrchestrator | undefined;
  getMessageIdToRequestId(): Map<string, string>;

  // UI 方法
  sendStateUpdate(): void;
  sendData(dataType: DataMessageType, payload: Record<string, unknown>): void;
  sendToast(message: string, level: NotifyLevel, duration?: number): void;
  sendExecutionStats(): void;
  sendOrchestratorMessage(params: {
    content?: string;
    messageType: 'progress' | 'error' | 'result' | 'text';
    metadata?: Record<string, unknown>;
    taskId?: string;
  }): void;
  appendLog(entry: LogEntry): void;
  postMessage(message: ExtensionToWebviewMessage): void;
  logMessageFlow(eventType: string, payload: unknown): void;

  // 请求管理
  resolveRequestTimeoutFromMessage(message: StandardMessage): void;
  clearRequestTimeout(requestId: string): void;
  interruptCurrentTask(options?: { silent?: boolean }): Promise<void>;
  tryResumePendingRecovery(): void;
  getMessageSnapshot(messageId: string): StandardMessage | null;
  getLiveSessionTimelineProjection(sessionId: string): SessionTimelineProjection | null;

  // 持久化
  persistStandardMessageToSession(sessionId: string, message: StandardMessage): void;
}

// ============================================================================
// EventBindingService
// ============================================================================

export class EventBindingService {
  // 工具授权状态（从 WVP 迁移）
  private toolAuthorizationCallbacks = new Map<string, (allowed: boolean) => void>();
  private toolAuthorizationQueue: Array<{ requestId: string; toolName: string; toolArgs: any }> = [];
  private activeToolAuthorizationRequestId: string | null = null;
  private activeToolAuthorizationTimer: NodeJS.Timeout | null = null;
  private readonly toolAuthorizationTimeoutMs = 60000;
  private readonly messageSessionByMessageId = new Map<string, string>();
  private readonly MAX_MESSAGE_SESSION_ENTRIES = 10000;
  private readonly pendingUpdatesByMessageId = new Map<string, StreamUpdate[]>();
  private readonly pendingUpdateTimers = new Map<string, NodeJS.Timeout>();
  private readonly MAX_PENDING_UPDATES_PER_MESSAGE = 200;
  private readonly PENDING_UPDATE_TIMEOUT_MS = 30000;
  private readonly liveSnapshotPersistTimers = new Map<string, NodeJS.Timeout>();
  private readonly pendingProjectionBroadcastTimers = new Map<string, NodeJS.Timeout>();
  private static readonly PROJECTION_BROADCAST_DEBOUNCE_MS = 200;
  private readonly LIVE_SNAPSHOT_PERSIST_DEBOUNCE_MS = 120;
  private lastAdapterErrorSignature = '';
  private lastAdapterErrorAt = 0;
  private readonly adapterErrorDedupWindowMs = 5000;

  constructor(private readonly ctx: EventBindingContext) {}

  /** 绑定全部事件（在 WVP 构造函数尾部调用） */
  bindAll(): void {
    this.setupAdapterEvents();
    this.setupMessageHubListeners();
    this.bindGlobalEvents();
  }

  /** 绑定 MissionOrchestrator 事件（MO 初始化后调用） */
  bindMissionEvents(): void {
    const mo = this.ctx.getMissionOrchestrator();
    if (!mo) return;

    const messageHub = this.ctx.getMessageHub();

    // Mission 生命周期
    mo.on('missionCreated', () => {
      this.ctx.sendStateUpdate();
    });

    mo.on('missionDeleted', () => {
      this.ctx.sendStateUpdate();
    });

    mo.on('missionStatusChanged', (data) => {
      const { mission, newStatus } = data;
      if (newStatus === 'failed') {
        this.ctx.sendData('missionFailed', {
          missionId: mission.id,
          error: t('eventBinding.missionFailed'),
          sessionId: this.ctx.getActiveSessionId(),
        });
      }
      this.ctx.sendStateUpdate();
      if (newStatus === 'completed' || newStatus === 'failed' || newStatus === 'cancelled') {
        this.ctx.tryResumePendingRecovery();
      }
    });

    mo.on('missionDeliveryChanged', () => {
      this.ctx.sendStateUpdate();
    });

    // Assignment 事件
    mo.on('assignmentStarted', (data) => {
      this.ctx.sendData('assignmentStarted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    mo.on('assignmentPlanned', (data) => {
      const assignmentId = data.assignmentId || generateEntityId('assignment');
      const todos = normalizeTodos(data.todos, assignmentId);
      this.ctx.sendData('assignmentPlanned', {
        missionId: data.missionId,
        assignmentId,
        todos,
        warnings: data.warnings,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    mo.on('assignmentCompleted', (data) => {
      this.ctx.sendData('assignmentCompleted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        success: data.success,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    // Worker Session 事件
    mo.on('workerSessionCreated', (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot }) => {
      this.ctx.sendData('workerSessionCreated', {
        sessionId: data.sessionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        trace: (data as { trace?: unknown }).trace,
      });
    });

    mo.on('workerSessionResumed', (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot; completedTodos: number }) => {
      this.ctx.sendData('workerSessionResumed', {
        sessionId: data.sessionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        completedTodos: data.completedTodos,
        trace: (data as { trace?: unknown }).trace,
      });
      const activeConversationSessionId = this.ctx.getActiveSessionId();
      messageHub.systemNotice(t('eventBinding.sessionResumed', { completedTodos: data.completedTodos }), {
        ...(activeConversationSessionId ? { sessionId: activeConversationSessionId } : {}),
        worker: data.workerId,
        extra: {
          workerSessionId: data.sessionId,
        },
      });
    });

    // Todo 事件
    mo.on('todoStarted', (data) => {
      this.ctx.sendData('todoStarted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    mo.on('todoCompleted', (data) => {
      this.ctx.sendData('todoCompleted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        output: data.output,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    mo.on('todoFailed', (data) => {
      this.ctx.sendData('todoFailed', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        error: data.error,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    // 动态 Todo
    mo.on('dynamicTodoAdded', (data) => {
      const assignmentId = data.assignmentId || generateEntityId('assignment');
      const normalizedTodo = normalizeTodos([data.todo], assignmentId)[0];
      if (!normalizedTodo) {
        logger.warn('动态 Todo 无效，已跳过发送', { assignmentId, missionId: data.missionId }, LogCategory.ORCHESTRATOR);
        return;
      }
      this.ctx.sendData('dynamicTodoAdded', {
        missionId: data.missionId,
        assignmentId,
        todo: normalizedTodo,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
      this.ctx.sendStateUpdate();
    });

    // 审批请求
    mo.on('approvalRequested', (data) => {
      const traceId = messageHub.getTraceId();
      const activeSessionId = this.ctx.getActiveSessionId();
      const interactionMsg = createInteractionMessage(
        {
          type: InteractionType.PERMISSION,
          requestId: `approval-${data.todoId}`,
          prompt: t('eventBinding.dynamicTodoApproval', { reason: data.reason }),
          required: true
        },
        'orchestrator',
        'orchestrator',
        traceId,
        {
          metadata: activeSessionId ? { sessionId: activeSessionId } : {},
        }
      );
      messageHub.sendMessage(interactionMsg);

      this.ctx.sendData('todoApprovalRequested', {
        missionId: data.missionId,
        todoId: data.todoId,
        reason: data.reason,
        trace: data.trace,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });
  }

  // ============================================================================
  // 工具授权（从 WVP 迁移的完整状态管理）
  // ============================================================================

  requestToolAuthorization(toolName: string, toolArgs: unknown): Promise<boolean> {
    const requestId = `tool-auth-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    return new Promise<boolean>((resolve) => {
      this.toolAuthorizationCallbacks.set(requestId, resolve);
      this.toolAuthorizationQueue.push({
        requestId,
        toolName,
        toolArgs,
      });
      this.pumpToolAuthorizationQueue();
    });
  }

  handleToolAuthorizationResponse(requestId: string | undefined, allowed: boolean): void {
    if (!requestId) {
      logger.warn('界面.工具授权.响应缺少请求ID', undefined, LogCategory.UI);
      this.ctx.sendToast(t('eventBinding.toolAuthMissingRequestId'), 'warning');
      return;
    }

    const callback = this.toolAuthorizationCallbacks.get(requestId);
    if (!callback) {
      logger.warn('界面.工具授权.回调不存在', { requestId }, LogCategory.UI);
      return;
    }

    this.toolAuthorizationCallbacks.delete(requestId);
    if (this.activeToolAuthorizationRequestId === requestId) {
      this.activeToolAuthorizationRequestId = null;
      this.clearActiveToolAuthorizationTimer();
    }

    callback(allowed);
    this.pumpToolAuthorizationQueue();
  }

  /** 清理所有待处理工具授权（dispose 时调用） */
  disposeToolAuthorization(): void {
    this.resetSessionRuntimeState();
  }

  flushLiveMessageSnapshots(options: { silent?: boolean } = {}): void {
    for (const messageId of Array.from(this.liveSnapshotPersistTimers.keys())) {
      const sessionId = this.messageSessionByMessageId.get(messageId);
      this.clearLiveMessageSnapshotPersist(messageId);
      if (!sessionId) {
        continue;
      }
      this.persistLiveMessageSnapshot(messageId, sessionId, options);
    }
  }

  resetSessionRuntimeState(): void {
    this.clearActiveToolAuthorizationTimer();
    this.activeToolAuthorizationRequestId = null;
    this.toolAuthorizationQueue = [];
    for (const callback of this.toolAuthorizationCallbacks.values()) {
      callback(false);
    }
    this.toolAuthorizationCallbacks.clear();
    for (const timer of this.pendingUpdateTimers.values()) {
      clearTimeout(timer);
    }
    this.pendingUpdateTimers.clear();
    this.pendingUpdatesByMessageId.clear();
    for (const timer of this.liveSnapshotPersistTimers.values()) {
      clearTimeout(timer);
    }
    this.liveSnapshotPersistTimers.clear();
    this.messageSessionByMessageId.clear();
  }

  // ============================================================================
  // 内部方法
  // ============================================================================

  private setupAdapterEvents(): void {
    this.ctx.getAdapterFactory().on(ADAPTER_EVENTS.ERROR, (error: Error) => {
      const rawError = error instanceof Error ? error.message : String(error ?? '');
      const normalized = toModelOriginUserMessage(rawError).trim() || t('eventBinding.adapterUnknownError');
      const modelOriginIssue = isModelOriginIssue(rawError);
      const level: NotifyLevel = modelOriginIssue ? 'warning' : 'error';
      const signature = `${level}:${normalized}`;
      const now = Date.now();

      if (
        this.lastAdapterErrorSignature === signature
        && now - this.lastAdapterErrorAt < this.adapterErrorDedupWindowMs
      ) {
        logger.debug('适配器错误.重复抑制', { signature }, LogCategory.LLM);
        return;
      }
      this.lastAdapterErrorSignature = signature;
      this.lastAdapterErrorAt = now;

      logger.error('适配器错误', {
        error: rawError,
        surfaced: normalized,
        modelOriginIssue,
      }, LogCategory.LLM);
      this.ctx.sendToast(t('eventBinding.adapterError', { error: normalized }), level);
    });
  }

  private setupMessageHubListeners(): void {
    const messageHub = this.ctx.getMessageHub();

    messageHub.on('unified:message', (message) => {
      const messageSessionId = this.resolveMessageSessionId(message);
      if (!messageSessionId) {
        logger.warn('界面.消息.丢弃_缺少会话标识', { messageId: message.id }, LogCategory.UI);
        return;
      }
      const messageAliasIds = this.resolveMessageAliasIds(message);
      const isLifecycleCard = message.type === MessageType.TASK_CARD || message.type === MessageType.INSTRUCTION;
      if (this.shouldPersistTimelineAnchorMessage(message)) {
        this.persistTimelineMessage(messageSessionId, message, {
          sendStateUpdate: isLifecycleCard,
        });
      }
      this.rememberMessageSessionAliases(messageAliasIds, messageSessionId);
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_MESSAGE,
        message,
        sessionId: messageSessionId
      });
      this.ctx.logMessageFlow('messageHub.standardMessage [SENT]', message);
      this.ctx.resolveRequestTimeoutFromMessage(message);
      // 先发送 message 锚点，再回放缓冲 update，确保前端严格遵守
      // “首次落位 -> 原位更新”的统一时间轴约束。
      for (const aliasId of messageAliasIds) {
        this.flushPendingUpdatesForMessage(aliasId, messageSessionId);
      }
    });

    messageHub.on('unified:update', (update) => {
      const updateSessionId = this.resolveUpdateSessionId(update);
      if (!updateSessionId) {
        const pendingAnchorId = this.resolvePendingUpdateAnchorId(update);
        if (pendingAnchorId) {
          this.bufferPendingUpdate(pendingAnchorId, update);
        }
        return;
      }
      this.rememberMessageSessionAliases(this.resolveUpdateAliasIds(update), updateSessionId);
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_UPDATE,
        update,
        sessionId: updateSessionId
      });
      this.ctx.logMessageFlow('messageHub.standardUpdate [SENT]', update);
      const reqId = this.ctx.getMessageIdToRequestId().get(update.messageId);
      if (reqId) {
        this.ctx.clearRequestTimeout(reqId);
      }
      this.scheduleLiveMessageSnapshotPersist(update.messageId, updateSessionId);
    });

    messageHub.on('unified:complete', (message) => {
      const completeAliasIds = this.resolveMessageAliasIds(message);
      const completeSessionId = this.resolveMessageSessionId(message)
        || completeAliasIds
          .map((aliasId) => this.messageSessionByMessageId.get(aliasId))
          .find((sessionId): sessionId is string => Boolean(sessionId))
        || null;
      if (!completeSessionId) {
        logger.warn('界面.消息.完成丢弃_缺少会话标识', { messageId: message.id }, LogCategory.UI);
        return;
      }
      this.rememberMessageSessionAliases(completeAliasIds, completeSessionId);
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_COMPLETE,
        message,
        sessionId: completeSessionId
      });
      this.ctx.logMessageFlow('messageHub.standardComplete [SENT]', message);
      this.ctx.resolveRequestTimeoutFromMessage(message);
      // 终态消息携带最终聚合内容；此前因缺少 session 归属而缓存的 update
      // 一旦走到 complete，就只应视为过期增量并直接清理。
      for (const aliasId of completeAliasIds) {
        this.clearPendingUpdatesForMessage(aliasId);
        this.clearLiveMessageSnapshotPersist(aliasId);
      }

      // 消息完成时统一收口到 session 持久化：
      // - timeline 内容进入会话时间轴
      // - notify 进入通知快照
      // - control/data 不持久化为主线程历史
      // 注意：lifecycle 消息（TASK_CARD/INSTRUCTION）已在 unified:message handler 中持久化，
      // 此处跳过，避免同一语义节点被重复写入 session.timeline。
      const isLifecycleCard = message.type === MessageType.TASK_CARD || message.type === MessageType.INSTRUCTION;
      if (!isLifecycleCard && message.id) {
        this.persistTimelineMessage(completeSessionId, message, {
          sendStateUpdate: Array.isArray(message.blocks) && message.blocks.length > 0,
        });
      }
    });

    messageHub.on(PROCESSING_EVENTS.STATE_CHANGED, (state) => {
      queueMicrotask(() => {
        this.ctx.sendData('processingStateChanged', {
          isProcessing: state.isProcessing,
          source: state.source,
          agent: state.agent,
          startedAt: state.startedAt,
          transitionKind: state.transitionKind,
        });
      });
    });
  }

  private bindGlobalEvents(): void {
    const messageHub = this.ctx.getMessageHub();
    const engine = this.ctx.getOrchestratorEngine();

    // 任务事件
    globalEventBus.on('task:created', () => this.ctx.sendStateUpdate());
    globalEventBus.on('task:state_changed', () => this.ctx.sendStateUpdate());
    globalEventBus.on('task:started', () => this.ctx.sendStateUpdate());
    globalEventBus.on('task:completed', () => this.ctx.sendStateUpdate());
    globalEventBus.on('task:failed', () => {
      this.ctx.sendStateUpdate();
    });
    globalEventBus.on('task:paused', () => this.ctx.sendStateUpdate());

    globalEventBus.on('task:cancelled', () => {
      this.ctx.sendStateUpdate();
      this.ctx.interruptCurrentTask();
    });

    globalEventBus.on('execution:stats_updated', () => this.ctx.sendExecutionStats());

    globalEventBus.on('orchestrator:phase_changed', (event) => {
      const data = event.data as { phase: string; isRunning?: boolean; timestamp?: number };
      if (data?.phase) {
        messageHub.phaseChange(
          data.phase,
          data.isRunning ?? engine.running,
          event.taskId || ''
        );
      }
    });

    globalEventBus.on('orchestrator:dependency_analysis', (event) => {
      const data = event.data as { message?: string };
      if (data?.message) {
        this.ctx.appendLog({
          level: 'info',
          message: data.message,
          source: 'orchestrator',
          timestamp: Date.now(),
        });
      }
    });

    globalEventBus.on('snapshot:created', () => this.ctx.sendStateUpdate());
    globalEventBus.on('snapshot:changed', () => this.ctx.sendStateUpdate());
    globalEventBus.on('snapshot:reverted', () => this.ctx.sendStateUpdate());

    // Worker 状态事件
    globalEventBus.on('worker:statusChanged', (event) => {
      const data = event.data as { worker: string; available: boolean; model?: string };
      this.ctx.sendStateUpdate();
      messageHub.workerStatus(data.worker, data.available, data.model);
    });

    globalEventBus.on('worker:healthCheck', () => this.ctx.sendStateUpdate());

    globalEventBus.on('worker:error', (event) => {
      const data = event.data as { worker: string; error: string };
      this.ctx.sendOrchestratorMessage({
        content: t('eventBinding.workerError', { worker: data.worker || 'Worker', error: data.error || 'Error' }),
        messageType: 'error',
        metadata: { worker: data.worker },
      });
    });

    globalEventBus.on('worker:session_event', (event) => {
      const data = event.data as {
        type?: string;
        worker?: WorkerSlot;
        role?: string;
        requestId?: string;
        reason?: string;
        error?: string;
      };
      const pieces = [
        data?.type || 'session',
        data?.worker ? `worker=${data.worker}` : '',
        data?.role ? `role=${data.role}` : '',
        data?.requestId ? `req=${data.requestId}` : '',
        data?.reason ? `reason=${data.reason}` : '',
        data?.error ? `error=${data.error}` : '',
      ].filter(Boolean);
      const level = data?.type?.includes('failed') ? 'error' : 'info';
      this.ctx.appendLog({
        level,
        message: pieces.join(' '),
        source: data?.worker ?? 'system',
        timestamp: Date.now(),
      });
    });

    // Mission 事件（延迟绑定）
    this.bindMissionEvents();
  }

  private clearActiveToolAuthorizationTimer(): void {
    if (this.activeToolAuthorizationTimer) {
      clearTimeout(this.activeToolAuthorizationTimer);
      this.activeToolAuthorizationTimer = null;
    }
  }

  private resolveMessageSessionId(message: StandardMessage): string | null {
    const binding = resolveStandardMessageSessionBinding(message);
    if (binding.hasConflict) {
      logger.warn('界面.消息.会话标识冲突_采用显式归属', {
        messageId: message.id,
        source: binding.source,
        metadataSessionId: binding.metadataSessionId,
        traceSessionId: binding.traceSessionId,
        dataPayloadSessionId: binding.dataPayloadSessionId,
        dataType: message.data?.dataType,
      }, LogCategory.UI);
    }
    return binding.sessionId;
  }

  private normalizeEntityId(raw: unknown): string {
    return typeof raw === 'string' ? raw.trim() : '';
  }

  private resolveMessageAliasIds(message: StandardMessage): string[] {
    const aliases = new Set<string>();
    const addAlias = (value: unknown): void => {
      const normalized = this.normalizeEntityId(value);
      if (normalized) {
        aliases.add(normalized);
      }
    };

    const metadata = message.metadata && typeof message.metadata === 'object'
      ? message.metadata as Record<string, unknown>
      : undefined;
    const subTaskCard = metadata?.subTaskCard && typeof metadata.subTaskCard === 'object'
      ? metadata.subTaskCard as Record<string, unknown>
      : undefined;

    addAlias(message.id);
    addAlias(metadata?.cardId);
    addAlias(metadata?.workerCardId);
    addAlias(subTaskCard?.workerCardId);

    return Array.from(aliases);
  }

  private resolveUpdateAliasIds(update: StreamUpdate): string[] {
    const aliases = new Set<string>();
    const messageId = this.normalizeEntityId(update.messageId);
    const cardId = this.normalizeEntityId(update.cardId);
    if (messageId) {
      aliases.add(messageId);
    }
    if (cardId) {
      aliases.add(cardId);
    }
    return Array.from(aliases);
  }

  private resolvePendingUpdateAnchorId(update: StreamUpdate): string {
    const cardId = this.normalizeEntityId(update.cardId);
    if (cardId) {
      return cardId;
    }
    return this.normalizeEntityId(update.messageId);
  }

  private resolveUpdateSessionId(update: StreamUpdate): string | null {
    for (const aliasId of this.resolveUpdateAliasIds(update)) {
      const sessionId = this.messageSessionByMessageId.get(aliasId);
      if (sessionId) {
        return sessionId;
      }
    }
    return null;
  }

  private rememberMessageSessionAliases(messageIds: string[], sessionId: string): void {
    for (const messageId of messageIds) {
      this.rememberMessageSession(messageId, sessionId);
    }
  }

  private rememberMessageSession(messageId: string, sessionId: string): void {
    const normalizedMessageId = this.normalizeEntityId(messageId);
    const normalizedSessionId = this.normalizeEntityId(sessionId);
    if (!normalizedMessageId || !normalizedSessionId) {
      return;
    }
    this.messageSessionByMessageId.set(normalizedMessageId, normalizedSessionId);
    if (this.messageSessionByMessageId.size <= this.MAX_MESSAGE_SESSION_ENTRIES) {
      return;
    }
    const oldestKey = this.messageSessionByMessageId.keys().next().value as string | undefined;
    if (oldestKey) {
      this.messageSessionByMessageId.delete(oldestKey);
    }
  }

  private bufferPendingUpdate(messageId: string, update: StreamUpdate): void {
    const normalizedMessageId = this.normalizeEntityId(messageId);
    if (!normalizedMessageId) {
      return;
    }
    const list = this.pendingUpdatesByMessageId.get(normalizedMessageId) || [];
    if (list.length >= this.MAX_PENDING_UPDATES_PER_MESSAGE) {
      list.shift();
    }
    list.push(update);
    this.pendingUpdatesByMessageId.set(normalizedMessageId, list);

    if (!this.pendingUpdateTimers.has(normalizedMessageId)) {
      const timer = setTimeout(() => {
        const dropped = this.pendingUpdatesByMessageId.get(normalizedMessageId)?.length || 0;
        this.pendingUpdatesByMessageId.delete(normalizedMessageId);
        this.pendingUpdateTimers.delete(normalizedMessageId);
        logger.warn('界面.消息.流式更新超时清理', {
          messageId: normalizedMessageId,
          dropped,
        }, LogCategory.UI);
      }, this.PENDING_UPDATE_TIMEOUT_MS);
      this.pendingUpdateTimers.set(normalizedMessageId, timer);
    }
  }

  private flushPendingUpdatesForMessage(messageId: string, sessionId: string): void {
    const updates = this.pendingUpdatesByMessageId.get(messageId);
    if (!updates || updates.length === 0) {
      return;
    }
    this.clearPendingUpdatesForMessage(messageId);
    for (const update of updates) {
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_UPDATE,
        update,
        sessionId,
      });
      this.ctx.logMessageFlow('messageHub.standardUpdate [FLUSHED]', update);
      const reqId = this.ctx.getMessageIdToRequestId().get(update.messageId);
      if (reqId) {
        this.ctx.clearRequestTimeout(reqId);
      }
    }
  }

  private clearPendingUpdatesForMessage(messageId: string): void {
    this.pendingUpdatesByMessageId.delete(messageId);
    const timer = this.pendingUpdateTimers.get(messageId);
    if (timer) {
      clearTimeout(timer);
      this.pendingUpdateTimers.delete(messageId);
    }
  }

  private scheduleLiveMessageSnapshotPersist(messageId: string, sessionId: string): void {
    this.clearLiveMessageSnapshotPersist(messageId);
    const timer = setTimeout(() => {
      this.liveSnapshotPersistTimers.delete(messageId);
      this.persistLiveMessageSnapshot(messageId, sessionId);
    }, this.LIVE_SNAPSHOT_PERSIST_DEBOUNCE_MS);
    this.liveSnapshotPersistTimers.set(messageId, timer);
  }

  private clearLiveMessageSnapshotPersist(messageId: string): void {
    const timer = this.liveSnapshotPersistTimers.get(messageId);
    if (!timer) {
      return;
    }
    clearTimeout(timer);
    this.liveSnapshotPersistTimers.delete(messageId);
  }

  private persistLiveMessageSnapshot(
    messageId: string,
    sessionId: string,
    options: { silent?: boolean } = {},
  ): void {
    const snapshot = this.ctx.getMessageSnapshot(messageId);
    if (!snapshot) {
      return;
    }
    const snapshotSessionId = this.resolveMessageSessionId(snapshot) || sessionId;
    if (!snapshotSessionId) {
      return;
    }
    this.persistTimelineMessage(snapshotSessionId, snapshot, {
      sendStateUpdate: options.silent !== true && Array.isArray(snapshot.blocks) && snapshot.blocks.length > 0,
    });
  }

  private shouldPersistTimelineAnchorMessage(message: StandardMessage): boolean {
    if (message.category !== MessageCategory.CONTENT) {
      return false;
    }
    if (message.type === MessageType.USER_INPUT) {
      return true;
    }
    if (message.type === MessageType.TASK_CARD || message.type === MessageType.INSTRUCTION) {
      return true;
    }
    if (message.metadata?.isPlaceholder === true) {
      return true;
    }
    const isStreamingAnchor = (
      message.lifecycle === MessageLifecycle.STARTED
      || message.lifecycle === MessageLifecycle.STREAMING
    );
    if (!isStreamingAnchor) {
      return false;
    }
    return messageHasRenderableTimelineContent({
      type: message.type,
      source: message.source,
      content: '',
      blocks: message.blocks,
      isStreaming: true,
      metadata: message.metadata as Record<string, unknown> | undefined,
    });
  }

  private persistTimelineMessage(
    sessionId: string,
    message: StandardMessage,
    options: { sendStateUpdate?: boolean; skipProjectionBroadcast?: boolean } = {},
  ): void {
    try {
      this.ctx.persistStandardMessageToSession(sessionId, message);
      if (!options.skipProjectionBroadcast) {
        this.scheduleTimelineProjectionBroadcast(sessionId);
      }
      if (options.sendStateUpdate === true) {
        this.ctx.sendStateUpdate();
      }
    } catch (error: any) {
      logger.warn('界面.时间轴消息.持久化失败', {
        messageId: message.id,
        messageType: message.type,
        sessionId,
        error: error?.message || String(error),
      }, LogCategory.UI);
    }
  }

  private scheduleTimelineProjectionBroadcast(sessionId: string): void {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    if (!normalizedSessionId) {
      return;
    }
    // debounce：同一 session 的多次调用合并为一次，避免高频 structuredClone 导致 CPU 飙高
    const existing = this.pendingProjectionBroadcastTimers.get(normalizedSessionId);
    if (existing) {
      clearTimeout(existing);
    }
    const timer = setTimeout(() => {
      this.pendingProjectionBroadcastTimers.delete(normalizedSessionId);
      const projection = this.ctx.getLiveSessionTimelineProjection(normalizedSessionId);
      if (!projection) {
        return;
      }
      this.ctx.sendData('timelineProjectionUpdated', {
        sessionId: normalizedSessionId,
        timelineProjection: projection,
      });
    }, EventBindingService.PROJECTION_BROADCAST_DEBOUNCE_MS);
    this.pendingProjectionBroadcastTimers.set(normalizedSessionId, timer);
  }

  private pumpToolAuthorizationQueue(): void {
    if (this.activeToolAuthorizationRequestId) return;
    const next = this.toolAuthorizationQueue.shift();
    if (!next) return;

    const messageHub = this.ctx.getMessageHub();
    const activeSessionId = this.ctx.getActiveSessionId();
    this.activeToolAuthorizationRequestId = next.requestId;

    const interactionMsg = createInteractionMessage(
      {
        type: InteractionType.PERMISSION,
        requestId: next.requestId,
        prompt: t('eventBinding.toolAuthRequest', { toolName: next.toolName }),
        required: true,
      },
      'orchestrator',
      'orchestrator',
      next.requestId,
      {
        metadata: activeSessionId ? { sessionId: activeSessionId } : {},
      }
    );
    messageHub.sendMessage(interactionMsg);

    this.ctx.sendData('toolAuthorizationRequest', {
      requestId: next.requestId,
      toolName: next.toolName,
      toolArgs: next.toolArgs,
    });

    this.clearActiveToolAuthorizationTimer();
    this.activeToolAuthorizationTimer = setTimeout(() => {
      const requestId = this.activeToolAuthorizationRequestId;
      if (!requestId) return;
      const callback = this.toolAuthorizationCallbacks.get(requestId);
      if (callback) {
        logger.warn('界面.工具授权.响应超时', { requestId }, LogCategory.UI);
        this.toolAuthorizationCallbacks.delete(requestId);
        callback(false);
      }
      this.activeToolAuthorizationRequestId = null;
      this.activeToolAuthorizationTimer = null;
      this.pumpToolAuthorizationQueue();
    }, this.toolAuthorizationTimeoutMs);
  }
}
