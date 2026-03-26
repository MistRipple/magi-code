/**
 * MessageFactory - 消息工厂（业务层）
 * 职责：提供语义化的消息 API，构造 StandardMessage，所有方法最终调用 pipeline.process()
 */

import { logger, LogCategory } from '../../logging';
import type { WorkerSlot, AgentType } from '../../types/agent-types';
import type { StandardMessage, MessageMetadata, ContentBlock, MessageSource, NotifyLevel, DataMessageType, NotifyPresentation } from '../../protocol/message-protocol';
import { MessageType, MessageLifecycle, MessageCategory, ControlMessageType, createStandardMessage, createControlMessage, createNotifyMessage, createDataMessage, createStreamingMessage } from '../../protocol/message-protocol';
import { classifyModelOriginIssue, toModelOriginUserMessage } from '../../errors/model-origin';
import { trackModelOriginEvent } from '../../errors/model-origin-observability';

interface WorkerLaneTaskCardSnapshot {
  taskId: string;
  title: string;
  worker?: WorkerSlot;
  status: 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
  summary?: string;
  fullSummary?: string;
  error?: string;
  failureCode?: string;
  recoverable?: boolean;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}

/** 子任务卡片载荷 - 用于 SubTaskCard 消息 */
export interface SubTaskCardPayload {
  id: string;
  title: string;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled' | 'skipped';
  worker: WorkerSlot;
  summary?: string;
  fullSummary?: string;
  error?: string;
  failureCode?: string;
  recoverable?: boolean;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
  /** 主会话 ID，必须用于固定 lifecycle 卡片归属，禁止依赖全局 trace 猜测 */
  sessionId?: string;
  missionId?: string;
  turnId?: string;
  /** 显式 requestId（避免依赖全局 requestContext，防止 Worker 并行执行期间竞态丢失） */
  requestId?: string;
  /** 生命周期卡片在时间轴中的语义锚点时间 */
  timelineAnchorTimestamp?: number;
  dispatchWaveId?: string;
  laneId?: string;
  workerCardId?: string;
  laneIndex?: number;
  laneTotal?: number;
  laneTaskIds?: string[];
  laneCurrentTaskId?: string;
  laneTasks?: Array<{
    taskId: string;
    title: string;
    status: 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
    dependsOn?: string[];
    isCurrent?: boolean;
  }>;
  laneTaskCards?: WorkerLaneTaskCardSnapshot[];
}

/** Worker 指令卡片元数据（用于 lane 聚合与可追踪性） */
export interface WorkerInstructionMetadata {
  laneCurrentTaskId?: string;
  laneTasks?: Array<{
    taskId: string;
    title: string;
    status: 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
    dependsOn?: string[];
    isCurrent?: boolean;
  }>;
  laneTaskCards?: WorkerLaneTaskCardSnapshot[];
  assignmentId?: string;
  /** 主会话 ID，必须用于固定 worker lane 卡片归属 */
  sessionId?: string;
  missionId?: string;
  /** 编排器轮次 ID，用于确定 lifecycle 卡片在时间轴中的排序位置 */
  turnId?: string;
  dispatchWaveId?: string;
  laneId?: string;
  workerCardId?: string;
  laneIndex?: number;
  laneTotal?: number;
  laneTaskIds?: string[];
  requestId?: string;
  /** 生命周期卡片在时间轴中的语义锚点时间 */
  timelineAnchorTimestamp?: number;
}

/** MessagePipeline 接口（协议层） */
export interface IMessagePipeline {
  process(message: StandardMessage): boolean;
  clearMessageState?(messageId: string): void;
  getRequestMessageId?(requestId: string): string | undefined;
}

export class MessageFactory {
  private pipeline: IMessagePipeline;
  private traceId: string;
  private sessionId: string | null;
  private requestId?: string;

  constructor(pipeline: IMessagePipeline, traceId?: string) {
    this.pipeline = pipeline;
    this.traceId = this.normalizeIdentifier(traceId) || this.generateTraceId();
    this.sessionId = this.normalizeIdentifier(traceId) || null;
  }

  // Trace 和 Request 上下文管理
  setTraceId(traceId: string): void { this.traceId = traceId; }
  getTraceId(): string { return this.traceId; }
  newTrace(): string { this.traceId = this.generateTraceId(); return this.traceId; }
  setSessionId(sessionId?: string | null): void { this.sessionId = this.normalizeIdentifier(sessionId) || null; }
  getSessionId(): string | null { return this.sessionId; }


  /** 发送进度消息 - 显示在主对话区 */
  progress(phase: string, content: string, options?: { percentage?: number; metadata?: MessageMetadata }): void {
    if (!content?.trim()) return;
    this.pipeline.process(this.createMessage({
      type: MessageType.PROGRESS,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: { phase, ...options?.metadata },
    }));
  }

  /** 发送结果消息 - 显示在主对话区 */
  result(content: string, options?: { success?: boolean; metadata?: MessageMetadata }): void {
    if (!content?.trim()) {
      logger.warn('MessageFactory.result.空内容跳过', undefined, LogCategory.SYSTEM);
      return;
    }
    const message = createStandardMessage({
      traceId: this.resolveTraceIdFromMetadata(options?.metadata || {}),
      category: MessageCategory.CONTENT,
      type: MessageType.RESULT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: this.enrichMetadata(options?.metadata || {}),
    });
    logger.info('MessageFactory.result.发送', { id: message.id, contentLength: content.length }, LogCategory.SYSTEM);
    this.pipeline.process(message);
  }

  /** 发送编排者分析/规划消息 - 显示在主对话区 */
  orchestratorMessage(content: string, options?: { type?: MessageType; metadata?: MessageMetadata }): void {
    const message = this.createMessage({
      type: options?.type || MessageType.TEXT,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: options?.metadata || {},
    });
    logger.info('MessageFactory.orchestratorMessage.发送', { id: message.id, contentLength: content.length }, LogCategory.SYSTEM);
    this.pipeline.process(message);
  }

  /** 发送子任务卡片 - 显示在主对话区 */
  subTaskCard(subTask: SubTaskCardPayload): void {
    const normalizedFailure = subTask.status === 'failed'
      ? this.normalizeFailureReason(subTask.error || subTask.summary || '执行失败')
      : null;
    const normalizedSubTask: SubTaskCardPayload = normalizedFailure
      ? {
        ...subTask,
        summary: normalizedFailure.userReason,
        error: normalizedFailure.userReason,
      }
      : subTask;

    const w = normalizedSubTask.worker;
    const statusContentMap: Record<SubTaskCardPayload['status'], string> = {
      completed: normalizedSubTask.summary ? `${w} 已完成：${normalizedSubTask.summary}` : `${w} 完成了任务`,
      failed: `${w} 执行遇到问题：${normalizedSubTask.summary || '执行失败'}`,
      pending: `${w} 排队中（等待前置任务）：${normalizedSubTask.title}`,
      cancelled: `${w} 已取消：${normalizedSubTask.title}`,
      skipped: `${w} 已跳过：${normalizedSubTask.title}`,
      running: `${w} 正在处理：${normalizedSubTask.title}`,
    };
    const content = statusContentMap[normalizedSubTask.status] || statusContentMap.running;
    const rawMissionId = typeof normalizedSubTask.missionId === 'string' ? normalizedSubTask.missionId.trim() : '';
    const rawTurnId = typeof normalizedSubTask.turnId === 'string' ? normalizedSubTask.turnId.trim() : '';
    const rawDispatchWaveId = typeof normalizedSubTask.dispatchWaveId === 'string'
      ? normalizedSubTask.dispatchWaveId.trim()
      : '';
    const rawLaneId = typeof normalizedSubTask.laneId === 'string' ? normalizedSubTask.laneId.trim() : '';
    const rawWorkerCardId = typeof normalizedSubTask.workerCardId === 'string'
      ? normalizedSubTask.workerCardId.trim()
      : '';
    const timelineAnchorTimestamp = typeof normalizedSubTask.timelineAnchorTimestamp === 'number'
      && Number.isFinite(normalizedSubTask.timelineAnchorTimestamp)
      && normalizedSubTask.timelineAnchorTimestamp > 0
      ? Math.floor(normalizedSubTask.timelineAnchorTimestamp)
      : undefined;

    // scope 优先级：requestId > missionId。
    // 注意：task card 的生命周期必然跨越多个 turn（从派发 pending 到最终 completed 往往经过多轮交互），
    // 绝对不能将 turnId 作为卡片唯一 ID 的一部分，否则在 task 跨回合完成时会导致重复渲染多张卡片。
    const explicitRequestId = typeof normalizedSubTask.requestId === 'string' ? normalizedSubTask.requestId.trim() : '';
    const scopeId = explicitRequestId || rawMissionId;
    const stableMessageId = rawWorkerCardId || (scopeId
      ? `subtask-card-${normalizedSubTask.id}-${scopeId}`
      : `subtask-card-${normalizedSubTask.id}`);
    const isFailed = normalizedSubTask.status === 'failed';
    const failureCode = typeof normalizedSubTask.failureCode === 'string' ? normalizedSubTask.failureCode.trim() : '';
    const modelOriginExtra = normalizedFailure?.isModelOrigin ? {
      modelOriginIssue: true,
      rawReason: normalizedFailure.rawReason,
      modelOriginKind: classifyModelOriginIssue(normalizedFailure.rawReason).kind || 'unknown',
      failureCode: failureCode || 'upstream_model_error',
    } : {};
    const dispatchFailureExtra = failureCode ? {
      failureCode,
      dispatchProtocolFailure: failureCode.startsWith('dispatch_'),
    } : {};

    this.pipeline.clearMessageState?.(stableMessageId);
    const normalizedLaneTaskCards = normalizeWorkerLaneTaskCards(normalizedSubTask.laneTaskCards);
    const subTaskMetadata = this.enrichMetadata({
      subTaskId: normalizedSubTask.id,
      assignmentId: normalizedSubTask.id,
      assignedWorker: normalizedSubTask.worker,
      isStatusMessage: true,
      ...(typeof normalizedSubTask.sessionId === 'string' && normalizedSubTask.sessionId.trim()
        ? { sessionId: normalizedSubTask.sessionId.trim() }
        : {}),
      ...(rawMissionId ? { missionId: rawMissionId } : {}),
      ...(rawTurnId ? { turnId: rawTurnId } : {}),
      ...(rawDispatchWaveId ? { dispatchWaveId: rawDispatchWaveId } : {}),
      ...(rawLaneId ? { laneId: rawLaneId } : {}),
      ...(rawWorkerCardId ? { workerCardId: rawWorkerCardId, cardId: rawWorkerCardId } : {}),
      ...(typeof normalizedSubTask.laneIndex === 'number' && Number.isFinite(normalizedSubTask.laneIndex)
        ? { laneIndex: Math.max(1, Math.floor(normalizedSubTask.laneIndex)) }
        : {}),
      ...(typeof normalizedSubTask.laneTotal === 'number' && Number.isFinite(normalizedSubTask.laneTotal)
        ? { laneTotal: Math.max(1, Math.floor(normalizedSubTask.laneTotal)) }
        : {}),
      ...(Array.isArray(normalizedSubTask.laneTaskIds) && normalizedSubTask.laneTaskIds.length > 0
        ? {
            laneTaskIds: normalizedSubTask.laneTaskIds
              .filter((taskId): taskId is string => typeof taskId === 'string' && taskId.trim().length > 0)
              .map((taskId) => taskId.trim()),
          }
        : {}),
      ...(typeof normalizedSubTask.laneCurrentTaskId === 'string' && normalizedSubTask.laneCurrentTaskId.trim()
        ? { laneCurrentTaskId: normalizedSubTask.laneCurrentTaskId.trim() }
        : {}),
      ...(Array.isArray(normalizedSubTask.laneTasks) && normalizedSubTask.laneTasks.length > 0
        ? {
            laneTasks: normalizedSubTask.laneTasks
              .filter((task): task is NonNullable<SubTaskCardPayload['laneTasks']>[number] => Boolean(task && typeof task === 'object'))
              .map((task) => ({
                taskId: task.taskId.trim(),
                title: task.title,
                status: task.status,
                ...(Array.isArray(task.dependsOn) ? { dependsOn: task.dependsOn } : {}),
                ...(typeof task.isCurrent === 'boolean' ? { isCurrent: task.isCurrent } : {}),
              }))
              .filter((task) => task.taskId.length > 0),
          }
        : {}),
      ...(normalizedLaneTaskCards.length > 0
        ? { laneTaskCards: normalizedLaneTaskCards }
        : {}),
      ...(timelineAnchorTimestamp ? { timelineAnchorTimestamp } : {}),
      subTaskCard: normalizedSubTask,
      ...(isFailed && failureCode ? { reason: failureCode } : {}),
      ...(isFailed ? { recoverable: normalizedSubTask.recoverable ?? true } : {}),
      ...((normalizedFailure?.isModelOrigin || failureCode) ? {
        extra: {
          ...modelOriginExtra,
          ...dispatchFailureExtra,
        },
      } : {}),
    });
    this.pipeline.process(createStandardMessage({
      id: stableMessageId,
      type: MessageType.TASK_CARD,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: subTaskMetadata,
      traceId: this.resolveTraceIdFromMetadata(subTaskMetadata),
      category: MessageCategory.CONTENT,
    }));
  }

  /** 发送任务分配宣告 - 主对话区 */
  taskAssignment(assignments: Array<{ worker: WorkerSlot; shortTitle: string }>, options?: { reason?: string }): void {
    if (assignments.length === 0) return;
    const workerList = assignments.map(a => `• ${a.worker}: ${a.shortTitle}`).join('\n');
    let content = assignments.length === 1
      ? `我将安排 ${assignments[0].worker} 执行：${assignments[0].shortTitle}`
      : `我将安排 ${assignments.length} 个 Worker 协作完成：\n${workerList}`;
    if (options?.reason) content += `\n\n> ${options.reason}`;
    this.orchestratorMessage(content, { metadata: { phase: 'task_assignment', isStatusMessage: true } });
  }

  /**
   * 创建一轮由插件主动发起的执行上下文。
   * 用于自动续跑/自动修复等内部轮次，确保后续 thinking/tool_call 能挂到独立 placeholder 下。
   */
  beginSyntheticRound(requestId: string, content: string, metadata?: MessageMetadata): {
    starterMessageId: string;
    placeholderMessageId: string;
  } {
    const normalizedRequestId = requestId.trim();
    const normalizedContent = content.trim();
    if (!normalizedRequestId || !normalizedContent) {
      throw new Error('beginSyntheticRound requires non-empty requestId/content');
    }

    const starterMessage = createStandardMessage({
      traceId: this.traceId,
      category: MessageCategory.CONTENT,
      type: MessageType.PROGRESS,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content: normalizedContent, isMarkdown: true }],
      metadata: this.enrichMetadata({
        ...metadata,
        requestId: normalizedRequestId,
        extra: {
          syntheticRequest: true,
          ...((metadata?.extra && typeof metadata.extra === 'object') ? metadata.extra : {}),
        },
      }),
    });

    const placeholderMessage = createStreamingMessage('orchestrator', 'orchestrator', this.traceId, {
      metadata: this.enrichMetadata({
        ...metadata,
        isPlaceholder: true,
        placeholderState: 'pending',
        requestId: normalizedRequestId,
        userMessageId: starterMessage.id,
        timelineAnchorTimestamp: starterMessage.timestamp,
        extra: {
          syntheticRequest: true,
          ...((metadata?.extra && typeof metadata.extra === 'object') ? metadata.extra : {}),
        },
      }),
    });

    starterMessage.metadata = {
      ...(starterMessage.metadata || {}),
      placeholderMessageId: placeholderMessage.id,
    };

    this.pipeline.process(starterMessage);
    this.pipeline.process(placeholderMessage);
    this.taskAccepted(normalizedRequestId);
    this.sendControl(ControlMessageType.TASK_STARTED, {
      requestId: normalizedRequestId,
      timestamp: Date.now(),
    });

    return {
      starterMessageId: starterMessage.id,
      placeholderMessageId: placeholderMessage.id,
    };
  }

  /** 发送 Worker 输出 - 路由到对应 Worker Tab */
  workerOutput(worker: WorkerSlot, content: string, options?: { blocks?: ContentBlock[]; metadata?: MessageMetadata }): void {
    this.pipeline.process(this.createMessage({
      type: MessageType.TEXT,
      source: 'worker',
      agent: worker as AgentType,
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: options?.blocks || [{ type: 'text', content, isMarkdown: true }],
      metadata: options?.metadata || {},
    }));
  }

  /** 发送 Worker 错误 - 强制路由到主对话区 */
  workerError(worker: WorkerSlot, content: string, options?: { metadata?: MessageMetadata }): void {
    const normalized = this.normalizeFailureReason(content || '执行失败');
    const metadata = this.buildFailureMetadata(normalized, options?.metadata || {});
    this.pipeline.process(this.createMessage({
      type: MessageType.ERROR,
      source: 'worker',
      agent: worker as AgentType,
      lifecycle: MessageLifecycle.FAILED,
      blocks: [{ type: 'text', content: normalized.userReason || '执行失败' }],
      metadata,
    }));
  }

  /** 发送 Worker 执行摘要 - Worker Tab 底部总结 */
  workerSummary(worker: WorkerSlot, content: string, options?: { metadata?: MessageMetadata }): void {
    if (!content?.trim()) return;
    this.pipeline.process(this.createMessage({
      type: MessageType.RESULT,
      source: 'worker',
      agent: worker as AgentType,
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: options?.metadata || {},
    }));
  }

  /** 发送任务说明到 Worker Tab */
  workerInstruction(worker: WorkerSlot, content: string, metadata?: WorkerInstructionMetadata): void {
    if (!content?.trim()) return;
    const workerCardId = metadata?.workerCardId?.trim();
    const stableMessageId = workerCardId || undefined;
    if (stableMessageId) {
      this.pipeline.clearMessageState?.(stableMessageId);
    }
    const normalizedLaneTaskCards = normalizeWorkerLaneTaskCards(metadata?.laneTaskCards);
    const instructionMetadata = this.enrichMetadata({
      ...metadata,
      ...(normalizedLaneTaskCards.length > 0 ? { laneTaskCards: normalizedLaneTaskCards } : {}),
      dispatchToWorker: true,
      worker,
      ...(typeof metadata?.sessionId === 'string' && metadata.sessionId.trim()
        ? { sessionId: metadata.sessionId.trim() }
        : {}),
      ...(typeof metadata?.timelineAnchorTimestamp === 'number'
        && Number.isFinite(metadata.timelineAnchorTimestamp)
        && metadata.timelineAnchorTimestamp > 0
        ? { timelineAnchorTimestamp: Math.floor(metadata.timelineAnchorTimestamp) }
        : {}),
      ...(workerCardId ? { cardId: workerCardId } : {}),
    });
    this.pipeline.process(createStandardMessage({
      id: stableMessageId,
      traceId: this.resolveTraceIdFromMetadata(instructionMetadata),
      category: MessageCategory.CONTENT,
      type: MessageType.INSTRUCTION,
      source: 'orchestrator',
      agent: worker as AgentType,
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: instructionMetadata,
    }));
  }

  /** 发送系统通知 */
  systemNotice(content: string, metadata?: MessageMetadata): void {
    if (!content?.trim()) return;
    this.pipeline.process(this.createMessage({
      type: MessageType.SYSTEM,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: { isStatusMessage: true, ...metadata },
    }));
  }

  /** 发送错误消息 */
  error(errorMsg: string, options?: { details?: Record<string, unknown>; recoverable?: boolean }): void {
    const normalized = this.normalizeFailureReason(errorMsg || '发生未知错误');
    const content = normalized.userReason || '发生未知错误';
    const metadata: MessageMetadata = {
      error: content,
      extra: options?.details,
      recoverable: options?.recoverable,
    };
    const mergedMetadata = this.buildFailureMetadata(normalized, metadata);
    this.pipeline.process(this.createMessage({
      type: MessageType.ERROR,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.FAILED,
      blocks: [{ type: 'text', content }],
      metadata: mergedMetadata,
    }));
  }

  /** 广播消息给所有订阅者 */
  broadcast(message: string | StandardMessage, options?: { target?: string; metadata?: MessageMetadata }): StandardMessage {
    const msg = typeof message === 'string'
      ? this.createMessage({
          type: MessageType.TEXT,
          source: 'orchestrator',
          agent: 'orchestrator',
          lifecycle: MessageLifecycle.COMPLETED,
          blocks: [{ type: 'text', content: message }],
          metadata: options?.metadata || {},
        })
      : message;
    this.pipeline.process(msg);
    return msg;
  }

  // 控制消息 API
  sendControl(controlType: ControlMessageType, payload: Record<string, unknown>): void {
    const sessionId = this.resolveCanonicalSessionId();
    this.pipeline.process(createControlMessage(controlType, payload, this.traceId, {
      metadata: sessionId ? { sessionId } : {},
    }));
  }

  notify(
    content: string,
    level: NotifyLevel = 'info',
    duration?: number,
    presentation?: NotifyPresentation,
  ): void {
    if (!content?.trim()) return;
    const sessionId = this.resolveCanonicalSessionId();
    this.pipeline.process(createNotifyMessage(content, level, this.traceId, duration, presentation, {
      metadata: sessionId ? { sessionId } : {},
    }));
  }

  data(dataType: DataMessageType, payload: Record<string, unknown>): void {
    const sessionId = this.resolveSessionIdFromPayload(payload);
    this.pipeline.process(createDataMessage(dataType, payload, this.resolveTraceIdFromPayload(payload), {
      metadata: sessionId ? { sessionId } : {},
    }));
  }

  // 便捷控制消息 API
  phaseChange(phase: string, isRunning: boolean, taskId?: string): void {
    this.sendControl(ControlMessageType.PHASE_CHANGED, { phase, isRunning, taskId, timestamp: Date.now() });
  }

  taskAccepted(requestId: string): void {
    this.sendControl(ControlMessageType.TASK_ACCEPTED, { requestId, timestamp: Date.now() });
  }

  taskRejected(requestId: string, reason: string): void {
    const normalized = this.normalizeFailureReason(reason || '任务被拒绝');
    this.sendControl(ControlMessageType.TASK_REJECTED, {
      requestId,
      reason: normalized.userReason,
      timestamp: Date.now(),
      ...(normalized.isModelOrigin ? {
        modelOriginIssue: true,
        rawReason: normalized.rawReason,
      } : {}),
    });
  }

  workerStatus(worker: string, available: boolean, model?: string): void {
    this.sendControl(ControlMessageType.WORKER_STATUS, { worker, available, model, timestamp: Date.now() });
  }

  // 内部方法
  private createMessage(params: {
    type: MessageType;
    source: MessageSource;
    agent: AgentType;
    lifecycle: MessageLifecycle;
    blocks: ContentBlock[];
    metadata: MessageMetadata;
    category?: MessageCategory;
  }): StandardMessage {
    const metadata = this.enrichMetadata(params.metadata);
    return createStandardMessage({
      ...params,
      traceId: this.resolveTraceIdFromMetadata(metadata),
      category: params.category || MessageCategory.CONTENT,
      metadata,
    });
  }

  private enrichMetadata(metadata: MessageMetadata): MessageMetadata {
    const sessionId = this.resolveCanonicalSessionId(metadata);
    if (!sessionId) {
      return metadata;
    }
    if (metadata.sessionId === sessionId) {
      return metadata;
    }
    return {
      ...metadata,
      sessionId,
    };
  }

  private resolveTraceIdFromMetadata(metadata?: MessageMetadata): string {
    return this.resolveCanonicalSessionId(metadata) || this.traceId;
  }

  private resolveTraceIdFromPayload(payload?: Record<string, unknown>): string {
    const sessionId = this.normalizeIdentifier(payload?.sessionId);
    if (sessionId) {
      return sessionId;
    }
    const traceId = this.normalizeIdentifier(payload?.traceId);
    return traceId || this.traceId;
  }

  private resolveSessionIdFromPayload(payload?: Record<string, unknown>): string | undefined {
    const sessionId = this.normalizeIdentifier(payload?.sessionId);
    if (sessionId) {
      return sessionId;
    }
    return this.resolveCanonicalSessionId();
  }

  private resolveCanonicalSessionId(metadata?: MessageMetadata): string | undefined {
    const metadataSessionId = this.normalizeIdentifier(metadata?.sessionId);
    if (metadataSessionId) {
      return metadataSessionId;
    }
    if (this.sessionId) {
      return this.sessionId;
    }
    return this.normalizeIdentifier(this.traceId);
  }

  private normalizeIdentifier(value: unknown): string | undefined {
    if (typeof value !== 'string') {
      return undefined;
    }
    const normalized = value.trim();
    return normalized.length > 0 ? normalized : undefined;
  }

  private normalizeFailureReason(reason: string): {
    rawReason: string;
    userReason: string;
    isModelOrigin: boolean;
    modelOriginKind?: string;
  } {
    const rawReason = (reason || '').trim();
    const userReason = toModelOriginUserMessage(rawReason).trim() || rawReason || '执行失败';
    const classified = classifyModelOriginIssue(rawReason);
    if (classified.isModelCause) {
      trackModelOriginEvent('surfaced', 'message-factory', rawReason, {
        surfacedReason: userReason,
      });
    }
    return {
      rawReason,
      userReason,
      isModelOrigin: classified.isModelCause,
      modelOriginKind: classified.kind,
    };
  }

  private buildFailureMetadata(
    normalized: { rawReason: string; userReason: string; isModelOrigin: boolean; modelOriginKind?: string },
    metadata: MessageMetadata,
  ): MessageMetadata {
    if (!normalized.isModelOrigin) {
      return metadata;
    }
    return {
      ...metadata,
      reason: typeof metadata.reason === 'string' && metadata.reason.trim()
        ? metadata.reason
        : 'upstream_model_error',
      recoverable: metadata.recoverable ?? true,
      extra: {
        ...(metadata.extra || {}),
        failureCode: (metadata.extra && typeof metadata.extra.failureCode === 'string')
          ? metadata.extra.failureCode
          : 'upstream_model_error',
        modelOriginIssue: true,
        modelOriginKind: normalized.modelOriginKind || 'unknown',
        rawReason: normalized.rawReason,
      },
    };
  }

  private generateTraceId(): string {
    return `trace_${Date.now()}_${Math.random().toString(36).substring(2, 9)}`;
  }
}

function normalizeWorkerLaneTaskCards(
  value: WorkerLaneTaskCardSnapshot[] | undefined,
): WorkerLaneTaskCardSnapshot[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value
    .filter((item): item is WorkerLaneTaskCardSnapshot => Boolean(item && typeof item === 'object'))
    .map((item) => {
      const taskId = typeof item.taskId === 'string' ? item.taskId.trim() : '';
      const title = typeof item.title === 'string' ? item.title.trim() : '';
      const status = typeof item.status === 'string' ? item.status.trim() : '';
      if (!taskId || !title || !status) {
        return null;
      }
      return {
        taskId,
        title,
        status: status as WorkerLaneTaskCardSnapshot['status'],
        ...(typeof item.worker === 'string' && item.worker.trim()
          ? { worker: item.worker.trim() as WorkerSlot }
          : {}),
        ...(typeof item.summary === 'string' && item.summary.trim()
          ? { summary: item.summary }
          : {}),
        ...(typeof item.fullSummary === 'string' && item.fullSummary.trim()
          ? { fullSummary: item.fullSummary }
          : {}),
        ...(typeof item.error === 'string' && item.error.trim()
          ? { error: item.error }
          : {}),
        ...(typeof item.failureCode === 'string' && item.failureCode.trim()
          ? { failureCode: item.failureCode.trim() }
          : {}),
        ...(typeof item.recoverable === 'boolean'
          ? { recoverable: item.recoverable }
          : {}),
        ...(Array.isArray(item.modifiedFiles) && item.modifiedFiles.length > 0
          ? {
              modifiedFiles: item.modifiedFiles
                .filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
                .map((file) => file.trim()),
            }
          : {}),
        ...(Array.isArray(item.createdFiles) && item.createdFiles.length > 0
          ? {
              createdFiles: item.createdFiles
                .filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
                .map((file) => file.trim()),
            }
          : {}),
        ...(typeof item.duration === 'number' && Number.isFinite(item.duration) && item.duration >= 0
          ? { duration: Math.floor(item.duration) }
          : {}),
      };
    })
    .filter((item): item is WorkerLaneTaskCardSnapshot => Boolean(item));
}
