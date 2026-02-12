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
import type {
  WorkerSlot,
  ExtensionToWebviewMessage,
  LogEntry,
} from '../types';
import {
  StandardMessage,
  DataMessageType,
  NotifyLevel,
  InteractionType,
  createInteractionMessage,
} from '../protocol/message-protocol';
import { ADAPTER_EVENTS, PROCESSING_EVENTS, WEBVIEW_MESSAGE_TYPES } from '../protocol/event-names';
import { globalEventBus } from '../events';
import type { IAdapterFactory } from '../adapters/adapter-factory-interface';
import type { MissionDrivenEngine } from '../orchestrator/core';
import type { MessageHub } from '../orchestrator/core/message-hub';
import type { MissionOrchestrator } from '../orchestrator/core';
import { normalizeTodos, generateEntityId } from '../orchestrator/mission/data-normalizer';
import { isAbortError } from '../errors';

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

    mo.on('missionStatusChanged', (data) => {
      const { mission, newStatus } = data;
      if (newStatus === 'failed') {
        this.ctx.sendData('missionFailed', {
          missionId: mission.id,
          error: '任务执行失败',
          sessionId: this.ctx.getActiveSessionId(),
        });
      }
      this.ctx.sendStateUpdate();
      if (newStatus === 'completed' || newStatus === 'failed' || newStatus === 'cancelled') {
        this.ctx.tryResumePendingRecovery();
      }
    });

    // Assignment 事件
    mo.on('assignmentStarted', (data) => {
      this.ctx.sendData('assignmentStarted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });

    mo.on('assignmentPlanned', (data) => {
      const assignmentId = data.assignmentId || generateEntityId('assignment');
      const todos = normalizeTodos(data.todos, assignmentId);
      this.ctx.sendData('assignmentPlanned', {
        missionId: data.missionId,
        assignmentId,
        todos,
        warnings: data.warnings,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });

    mo.on('assignmentCompleted', (data) => {
      this.ctx.sendData('assignmentCompleted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        success: data.success,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });

    // Worker Session 事件
    mo.on('workerSessionCreated', (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot }) => {
      this.ctx.sendData('workerSessionCreated', {
        sessionId: data.sessionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
      });
    });

    mo.on('workerSessionResumed', (data: { sessionId: string; assignmentId: string; workerId: WorkerSlot; completedTodos: number }) => {
      this.ctx.sendData('workerSessionResumed', {
        sessionId: data.sessionId,
        assignmentId: data.assignmentId,
        workerId: data.workerId,
        completedTodos: data.completedTodos,
      });
      messageHub.systemNotice(`Session 已恢复，继续执行 ${data.completedTodos} 个已完成的 Todo`, {
        sessionId: data.sessionId,
        worker: data.workerId,
      });
    });

    // Todo 事件
    mo.on('todoStarted', (data) => {
      this.ctx.sendData('todoStarted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });

    mo.on('todoCompleted', (data) => {
      this.ctx.sendData('todoCompleted', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        output: data.output,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });

    mo.on('todoFailed', (data) => {
      this.ctx.sendData('todoFailed', {
        missionId: data.missionId,
        assignmentId: data.assignmentId,
        todoId: data.todoId,
        error: data.error,
        sessionId: this.ctx.getActiveSessionId(),
      });
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
        sessionId: this.ctx.getActiveSessionId(),
      });
    });

    // 审批请求
    mo.on('approvalRequested', (data) => {
      const traceId = messageHub.getTraceId();
      const interactionMsg = createInteractionMessage(
        {
          type: InteractionType.PERMISSION,
          requestId: `approval-${data.todoId}`,
          prompt: `**动态任务审批**\n\n原因: ${data.reason}\n\n请决定是否批准。`,
          required: true
        },
        'orchestrator',
        'orchestrator',
        traceId
      );
      messageHub.sendMessage(interactionMsg);

      this.ctx.sendData('todoApprovalRequested', {
        missionId: data.missionId,
        todoId: data.todoId,
        reason: data.reason,
        sessionId: this.ctx.getActiveSessionId(),
      });
    });
  }

  // ============================================================================
  // 工具授权（从 WVP 迁移的完整状态管理）
  // ============================================================================

  handleToolAuthorizationResponse(requestId: string | undefined, allowed: boolean): void {
    if (!requestId) {
      logger.warn('界面.工具授权.响应缺少请求ID', undefined, LogCategory.UI);
      this.ctx.sendToast('工具授权响应缺少请求标识，已忽略', 'warning');
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
    this.clearActiveToolAuthorizationTimer();
    this.activeToolAuthorizationRequestId = null;
    this.toolAuthorizationQueue = [];
    for (const callback of this.toolAuthorizationCallbacks.values()) {
      callback(false);
    }
    this.toolAuthorizationCallbacks.clear();
  }

  // ============================================================================
  // 内部方法
  // ============================================================================

  private setupAdapterEvents(): void {
    this.ctx.getAdapterFactory().on(ADAPTER_EVENTS.ERROR, (error: Error) => {
      logger.error('适配器错误', { error: error.message }, LogCategory.LLM);
    });
  }

  private setupMessageHubListeners(): void {
    const messageHub = this.ctx.getMessageHub();

    messageHub.on('unified:message', (message) => {
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_MESSAGE,
        message,
        sessionId: this.ctx.getActiveSessionId()
      });
      this.ctx.logMessageFlow('messageHub.standardMessage [SENT]', message);
      this.ctx.resolveRequestTimeoutFromMessage(message);
    });

    messageHub.on('unified:update', (update) => {
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_UPDATE,
        update,
        sessionId: this.ctx.getActiveSessionId()
      });
      this.ctx.logMessageFlow('messageHub.standardUpdate [SENT]', update);
      const reqId = this.ctx.getMessageIdToRequestId().get(update.messageId);
      if (reqId) {
        this.ctx.clearRequestTimeout(reqId);
      }
    });

    messageHub.on('unified:complete', (message) => {
      this.ctx.postMessage({
        type: WEBVIEW_MESSAGE_TYPES.UNIFIED_COMPLETE,
        message,
        sessionId: this.ctx.getActiveSessionId()
      });
      this.ctx.logMessageFlow('messageHub.standardComplete [SENT]', message);
      this.ctx.resolveRequestTimeoutFromMessage(message);
    });

    messageHub.on(PROCESSING_EVENTS.STATE_CHANGED, (state) => {
      this.ctx.sendData('processingStateChanged', {
        isProcessing: state.isProcessing,
        source: state.source,
        agent: state.agent,
        startedAt: state.startedAt,
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
    globalEventBus.on('task:failed', (event) => {
      this.ctx.sendStateUpdate();
      const data = event.data as { error?: string | object; stack?: string };
      let errorMsg = '任务执行失败';
      if (data?.error) {
        if (typeof data.error === 'string') {
          errorMsg = data.error;
        } else if (typeof data.error === 'object') {
          const errObj = data.error as { message?: string; error?: string };
          errorMsg = errObj.message || errObj.error || JSON.stringify(data.error);
        }
      }
      if (isAbortError(errorMsg)) return;
      this.ctx.sendOrchestratorMessage({
        content: errorMsg,
        messageType: 'error',
        taskId: event.taskId,
        metadata: { error: errorMsg, stack: data?.stack },
      });
    });

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
        content: `${data.worker || 'Worker'}: ${data.error || '发生错误'}`,
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

    // 工具授权请求
    globalEventBus.on('tool:authorization_request', (event) => {
      const data = event.data as {
        toolName: string;
        toolArgs: any;
        callback: (allowed: boolean) => void;
      };
      const requestId = `tool-auth-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      this.toolAuthorizationCallbacks.set(requestId, data.callback);
      this.toolAuthorizationQueue.push({
        requestId,
        toolName: data.toolName,
        toolArgs: data.toolArgs,
      });
      this.pumpToolAuthorizationQueue();
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

  private pumpToolAuthorizationQueue(): void {
    if (this.activeToolAuthorizationRequestId) return;
    const next = this.toolAuthorizationQueue.shift();
    if (!next) return;

    const messageHub = this.ctx.getMessageHub();
    this.activeToolAuthorizationRequestId = next.requestId;

    const interactionMsg = createInteractionMessage(
      {
        type: InteractionType.PERMISSION,
        requestId: next.requestId,
        prompt: `工具授权请求: ${next.toolName}`,
        required: true,
      },
      'orchestrator',
      'orchestrator',
      next.requestId,
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
