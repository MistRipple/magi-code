/**
 * MessageFactory - 消息工厂（业务层）
 * 职责：提供语义化的消息 API，构造 StandardMessage，所有方法最终调用 pipeline.process()
 */

import { logger, LogCategory } from '../../logging';
import type { WorkerSlot, AgentType } from '../../types/agent-types';
import type { StandardMessage, MessageMetadata, ContentBlock, MessageSource, NotifyLevel, DataMessageType } from '../../protocol/message-protocol';
import { MessageType, MessageLifecycle, MessageCategory, ControlMessageType, createStandardMessage, createControlMessage, createNotifyMessage, createDataMessage } from '../../protocol/message-protocol';
import { classifyModelOriginIssue, toModelOriginUserMessage } from '../../errors/model-origin';
import { trackModelOriginEvent } from '../../errors/model-origin-observability';

/** 子任务卡片载荷 - 用于 SubTaskCard 消息 */
export interface SubTaskCardPayload {
  id: string;
  title: string;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'stopped' | 'skipped';
  worker: WorkerSlot;
  summary?: string;
  error?: string;
  failureCode?: string;
  recoverable?: boolean;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
  missionId?: string;
  turnId?: string;
  /** 显式 requestId（避免依赖全局 requestContext，防止 Worker 并行执行期间竞态丢失） */
  requestId?: string;
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
  assignmentId?: string;
  missionId?: string;
  laneId?: string;
  laneCardId?: string;
  laneIndex?: number;
  laneTotal?: number;
  laneTaskIds?: string[];
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
  private requestId?: string;

  constructor(pipeline: IMessagePipeline, traceId?: string) {
    this.pipeline = pipeline;
    this.traceId = traceId || this.generateTraceId();
  }

  // Trace 和 Request 上下文管理
  setTraceId(traceId: string): void { this.traceId = traceId; }
  getTraceId(): string { return this.traceId; }
  newTrace(): string { this.traceId = this.generateTraceId(); return this.traceId; }
  setRequestContext(requestId?: string): void { this.requestId = requestId; }
  getRequestContext(): string | undefined { return this.requestId; }

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
    const requestId = (options?.metadata as { requestId?: string } | undefined)?.requestId || this.requestId;
    const reuseMessageId = requestId ? this.pipeline.getRequestMessageId?.(requestId) : undefined;
    const message = createStandardMessage({
      id: reuseMessageId,
      traceId: this.traceId,
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
      stopped: `${w} 已停止：${normalizedSubTask.title}`,
      skipped: `${w} 已跳过：${normalizedSubTask.title}`,
      running: `${w} 正在处理：${normalizedSubTask.title}`,
    };
    const content = statusContentMap[normalizedSubTask.status] || statusContentMap.running;
    const rawMissionId = typeof normalizedSubTask.missionId === 'string' ? normalizedSubTask.missionId.trim() : '';
    const rawTurnId = typeof normalizedSubTask.turnId === 'string' ? normalizedSubTask.turnId.trim() : '';

    // scope 优先级：requestId > missionId。
    // 注意：task card 的生命周期必然跨越多个 turn（从派发 pending 到最终 completed 往往经过多轮交互），
    // 绝对不能将 turnId 作为卡片唯一 ID 的一部分，否则在 task 跨回合完成时会导致重复渲染多张卡片。
    const scopeId = this.requestId || rawMissionId;
    const stableMessageId = scopeId
      ? `subtask-card-${normalizedSubTask.id}-${scopeId}`
      : `subtask-card-${normalizedSubTask.id}`;
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
    this.pipeline.process(createStandardMessage({
      id: stableMessageId,
      type: MessageType.TASK_CARD,
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: this.enrichMetadata({
        subTaskId: normalizedSubTask.id,
        assignmentId: normalizedSubTask.id,
        assignedWorker: normalizedSubTask.worker,
        isStatusMessage: true,
        ...(rawMissionId ? { missionId: rawMissionId } : {}),
        ...(rawTurnId ? { turnId: rawTurnId } : {}),
        subTaskCard: normalizedSubTask,
        ...(isFailed && failureCode ? { reason: failureCode } : {}),
        ...(isFailed ? { recoverable: normalizedSubTask.recoverable ?? true } : {}),
        ...((normalizedFailure?.isModelOrigin || failureCode) ? {
          extra: {
            ...modelOriginExtra,
            ...dispatchFailureExtra,
          },
        } : {}),
      }),
      traceId: this.traceId,
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
    const laneCardId = metadata?.laneCardId?.trim();
    const stableMessageId = laneCardId || undefined;
    if (stableMessageId) {
      this.pipeline.clearMessageState?.(stableMessageId);
    }
    this.pipeline.process(createStandardMessage({
      id: stableMessageId,
      traceId: this.traceId,
      category: MessageCategory.CONTENT,
      type: MessageType.INSTRUCTION,
      source: 'orchestrator',
      agent: worker as AgentType,
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content, isMarkdown: true }],
      metadata: this.enrichMetadata({
        ...metadata,
        dispatchToWorker: true,
        worker,
        ...(laneCardId ? { cardId: laneCardId } : {}),
      }),
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
    this.pipeline.process(createControlMessage(controlType, payload, this.traceId));
  }

  notify(content: string, level: NotifyLevel = 'info', duration?: number): void {
    if (!content?.trim()) return;
    this.pipeline.process(createNotifyMessage(content, level, this.traceId, duration));
  }

  data(dataType: DataMessageType, payload: Record<string, unknown>): void {
    this.pipeline.process(createDataMessage(dataType, payload, this.traceId));
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
    return createStandardMessage({
      ...params,
      traceId: this.traceId,
      category: params.category || MessageCategory.CONTENT,
      metadata: this.enrichMetadata(params.metadata),
    });
  }

  private enrichMetadata(metadata: MessageMetadata): MessageMetadata {
    if (this.requestId && !metadata.requestId) {
      return { ...metadata, requestId: this.requestId };
    }
    return metadata;
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
