/**
 * MessagePipeline - 消息协议层
 *
 * 负责消息校验、去重、节流、生命周期管理
 * 接收 MessageBus 作为构造参数，验证通过的消息会发射到 Bus
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import type { StandardMessage, ContentBlock, StreamUpdate, MessageSource } from '../../protocol/message-protocol';
import { MessageType, MessageLifecycle, MessageCategory, createStandardMessage } from '../../protocol/message-protocol';

/** Pipeline 配置 */
export interface PipelineConfig {
  enabled: boolean;
  minStreamInterval: number;
  retentionTime: number;
  debug: boolean;
}

interface MessageState {
  message: StandardMessage | null;
  createdAt: number;
  lastSentAt: number;
  lastStreamAt: number;
  completed: boolean;
  cardId: string;
  lastCardStreamSeq: number;
  finalCardStreamSeq?: number;
}

interface SealedCardState {
  cardId: string;
  finalStreamSeq: number;
  sealedAt: number;
  source: MessageSource;
  agent: StandardMessage['agent'];
  traceId: string;
  requestId?: string;
}

interface DeadLetterEntry {
  reason: 'sealed_duplicate' | 'sealed_late' | 'out_of_order_update';
  cardId: string;
  messageId: string;
  cardStreamSeq: number;
  eventSeq: number;
  timestamp: number;
  requestId?: string;
}

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
  createdAt: number;
}

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

export interface ProcessingState {
  isProcessing: boolean;
  source: MessageSource | null;
  agent: string | null;
  startedAt: number | null;
}

const DEFAULT_CONFIG: PipelineConfig = {
  enabled: true,
  minStreamInterval: 0,
  retentionTime: 5 * 60 * 1000,
  debug: false,
};

export class MessagePipeline {
  private bus: EventEmitter;
  private config: PipelineConfig;
  private messageStates: Map<string, MessageState> = new Map();
  private sealedCards: Map<string, SealedCardState> = new Map();
  private cardStreamSeqCounters: Map<string, number> = new Map();
  private deadLetters: DeadLetterEntry[] = [];
  private eventSeqCounter = 0;
  private processingState: ProcessingState = { isProcessing: false, source: null, agent: null, startedAt: null };
  private processingMessageIds: Set<string> = new Set();
  private requestMessageStats: Map<string, RequestMessageStats> = new Map();
  private requestMessageIdMap: Map<string, string> = new Map();
  private streamBuffers: Map<string, { text: string; lastBlocks?: ContentBlock[] }> = new Map();
  private cleanupTimer: NodeJS.Timeout | null = null;

  constructor(bus: EventEmitter, config?: Partial<PipelineConfig>) {
    this.bus = bus;
    this.config = { ...DEFAULT_CONFIG, ...config };
    this.startCleanupTimer();
  }

  private generateEventId(prefix: string): string {
    return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  private nextEventSeq(): number {
    this.eventSeqCounter += 1;
    return this.eventSeqCounter;
  }

  /**
   * 统一收口 eventSeq：
   * - 外部提供的序号只有在大于当前计数器时才接纳
   * - 否则由 Pipeline 重新分配，保证全局单调递增
   */
  private resolveEventSeq(explicitSeq?: number): number {
    if (typeof explicitSeq === 'number' && Number.isFinite(explicitSeq)) {
      const normalized = Math.floor(explicitSeq);
      if (normalized > this.eventSeqCounter) {
        this.eventSeqCounter = normalized;
        return normalized;
      }
    }
    return this.nextEventSeq();
  }

  private resolveCardId(messageId: string, metadata?: StandardMessage['metadata']): string {
    if (metadata?.cardId && metadata.cardId.trim()) {
      return metadata.cardId.trim();
    }
    return messageId;
  }

  private nextCardStreamSeq(cardId: string): number {
    const current = this.cardStreamSeqCounters.get(cardId) || 0;
    const next = current + 1;
    this.cardStreamSeqCounters.set(cardId, next);
    return next;
  }

  private ensureMessageEnvelope(message: StandardMessage): StandardMessage {
    const metadata = message.metadata || {};
    const cardId = this.resolveCardId(message.id, metadata);
    const cardStreamSeq = typeof metadata.cardStreamSeq === 'number' && Number.isFinite(metadata.cardStreamSeq)
      ? metadata.cardStreamSeq
      : this.nextCardStreamSeq(cardId);
    const knownCardSeq = this.cardStreamSeqCounters.get(cardId) || 0;
    if (cardStreamSeq > knownCardSeq) {
      this.cardStreamSeqCounters.set(cardId, cardStreamSeq);
    }
    return {
      ...message,
      eventId: message.eventId || this.generateEventId('evt'),
      eventSeq: this.resolveEventSeq(message.eventSeq),
      metadata: {
        ...metadata,
        cardId,
        cardStreamSeq,
      },
    };
  }

  private ensureUpdateEnvelope(update: StreamUpdate, fallbackCardId?: string): StreamUpdate {
    const cardId = (update.cardId && update.cardId.trim())
      || (fallbackCardId && fallbackCardId.trim())
      || update.messageId;
    const cardStreamSeq = typeof update.cardStreamSeq === 'number' && Number.isFinite(update.cardStreamSeq)
      ? update.cardStreamSeq
      : this.nextCardStreamSeq(cardId);
    const knownCardSeq = this.cardStreamSeqCounters.get(cardId) || 0;
    if (cardStreamSeq > knownCardSeq) {
      this.cardStreamSeqCounters.set(cardId, cardStreamSeq);
    }
    return {
      ...update,
      cardId,
      cardStreamSeq,
      eventId: update.eventId || this.generateEventId('upd'),
      eventSeq: this.resolveEventSeq(update.eventSeq),
    };
  }

  private recordDeadLetter(entry: DeadLetterEntry): void {
    this.deadLetters.push(entry);
    if (this.deadLetters.length > 2000) {
      this.deadLetters.splice(0, this.deadLetters.length - 2000);
    }
    logger.warn('MessagePipeline.dead_letter', entry, LogCategory.SYSTEM);
  }

  getDeadLetterCount(): number {
    return this.deadLetters.length;
  }

  private sealCard(message: StandardMessage, state?: MessageState): void {
    const cardId = this.resolveCardId(message.id, message.metadata);
    const finalStreamSeq = typeof message.metadata?.cardStreamSeq === 'number'
      ? message.metadata.cardStreamSeq
      : (state?.lastCardStreamSeq || 0);
    const sealed: SealedCardState = {
      cardId,
      finalStreamSeq,
      sealedAt: Date.now(),
      source: message.source,
      agent: message.agent,
      traceId: message.traceId,
      requestId: message.metadata?.requestId,
    };
    this.sealedCards.set(cardId, sealed);
    if (state) {
      state.finalCardStreamSeq = finalStreamSeq;
    }
  }

  private addActiveMessage(id: string, message: StandardMessage, ts: number): void {
    const cardId = this.resolveCardId(id, message.metadata);
    this.messageStates.set(id, {
      message,
      createdAt: ts,
      lastSentAt: ts,
      lastStreamAt: ts,
      completed: false,
      cardId,
      lastCardStreamSeq: message.metadata?.cardStreamSeq || 0,
    });
  }

  private markMessageComplete(id: string, message: StandardMessage, ts: number): void {
    const state = this.messageStates.get(id);
    if (state) {
      state.message = message;
      state.lastSentAt = ts;
      state.completed = true;
      if (typeof message.metadata?.cardStreamSeq === 'number') {
        state.lastCardStreamSeq = Math.max(state.lastCardStreamSeq, message.metadata.cardStreamSeq);
      }
      this.sealCard(message, state);
    } else {
      this.sealCard(message);
    }
    this.streamBuffers.delete(id);
  }

  private hasActiveMessages(): boolean {
    for (const state of this.messageStates.values()) { if (!state.completed) return true; }
    return false;
  }

  clearMessageState(messageId: string): void {
    const state = this.messageStates.get(messageId);
    if (state?.cardId) {
      this.sealedCards.delete(state.cardId);
      this.cardStreamSeqCounters.delete(state.cardId);
    }
    this.messageStates.delete(messageId);
  }

  getRequestMessageStats(requestId: string): RequestMessageSummary | undefined {
    const stats = this.requestMessageStats.get(requestId);
    return stats ? this.toRequestSummary(stats) : undefined;
  }

  finalizeRequestContext(requestId: string): RequestMessageSummary | undefined {
    const stats = this.requestMessageStats.get(requestId);
    if (!stats) return undefined;
    this.requestMessageStats.delete(requestId);
    this.requestMessageIdMap.delete(requestId);
    return this.toRequestSummary(stats);
  }

  getRequestMessageId(requestId: string): string | undefined { return this.requestMessageIdMap.get(requestId); }
  setRequestMessageId(requestId: string, messageId: string): void { this.requestMessageIdMap.set(requestId, messageId); }

  process(message: StandardMessage, requestId?: string): boolean {
    if (message.id && this.processingMessageIds.has(message.id)) { this.debugLog('跳过消息 [RE-ENTRANT]', message.id); return false; }
    if (message.id) this.processingMessageIds.add(message.id);
    try { return this.doProcess(message, requestId); }
    finally { if (message.id) this.processingMessageIds.delete(message.id); }
  }

  processUpdate(update: StreamUpdate): boolean {
    if (!update.messageId?.trim()) throw new Error('[MessagePipeline] StreamUpdate missing messageId');
    if (Array.isArray(update.blocks)) {
      const invalid = update.blocks.filter(b => !b || typeof b !== 'object' || typeof (b as any).type !== 'string');
      if (invalid.length > 0) throw new Error(`[MessagePipeline] Invalid update blocks: ${update.messageId}`);
    }
    // 消息 ID 由生产者（adapter/normalizer）在 startStreamWithContext 时设置为正确值
    // Pipeline 不做 ID 映射，直接使用原始 messageId
    const existingState = this.messageStates.get(update.messageId);
    const effectiveUpdate = this.ensureUpdateEnvelope(update, existingState?.cardId);
    const cardId = effectiveUpdate.cardId || update.messageId;
    const sealed = this.sealedCards.get(cardId);

    if (sealed) {
      // sealed card 不应再收到任何 update，统一记录 dead letter
      const incomingSeq = effectiveUpdate.cardStreamSeq || 0;
      this.recordDeadLetter({
        reason: incomingSeq <= sealed.finalStreamSeq ? 'sealed_duplicate' : 'sealed_late',
        cardId,
        messageId: effectiveUpdate.messageId,
        cardStreamSeq: incomingSeq,
        eventSeq: effectiveUpdate.eventSeq || 0,
        timestamp: Date.now(),
        requestId: sealed.requestId,
      });
      return false;
    }

    if (!this.config.enabled) {
      this.updateStreamBufferFromUpdate(effectiveUpdate);
      this.safeEmit('unified:update', effectiveUpdate);
      return true;
    }

    const now = Date.now();
    this.updateStreamBufferFromUpdate(effectiveUpdate);
    const hadState = Boolean(existingState);
    let state = existingState;
    if (!state) {
      state = {
        message: null,
        createdAt: now,
        lastSentAt: 0,
        lastStreamAt: 0,
        completed: false,
        cardId,
        lastCardStreamSeq: effectiveUpdate.cardStreamSeq || 0,
      };
      this.messageStates.set(effectiveUpdate.messageId, state);
    }
    if (state.completed) {
      // 新架构：endStream 后不应再有 UPDATE 到同一 messageId
      logger.warn('MessagePipeline.已完成_收到_UPDATE', {
        messageId: update.messageId,
        updateType: update.updateType,
      }, LogCategory.SYSTEM);
      return false;
    }
    if (
      hadState
      && typeof effectiveUpdate.cardStreamSeq === 'number'
      && effectiveUpdate.cardStreamSeq <= state.lastCardStreamSeq
    ) {
      this.recordDeadLetter({
        reason: 'out_of_order_update',
        cardId,
        messageId: effectiveUpdate.messageId,
        cardStreamSeq: effectiveUpdate.cardStreamSeq,
        eventSeq: effectiveUpdate.eventSeq || 0,
        timestamp: Date.now(),
      });
      return false;
    }
    if (now - state.lastStreamAt < this.config.minStreamInterval) return false;
    state.lastStreamAt = now;
    if (typeof effectiveUpdate.cardStreamSeq === 'number') {
      state.lastCardStreamSeq = Math.max(state.lastCardStreamSeq, effectiveUpdate.cardStreamSeq);
    }
    this.safeEmit('unified:update', effectiveUpdate);
    return true;
  }

  getProcessingState(): ProcessingState { return { ...this.processingState }; }

  forceProcessingState(isProcessing: boolean): void {
    if (!isProcessing) {
      const now = Date.now();
      for (const state of this.messageStates.values()) { if (!state.completed) { state.completed = true; state.lastSentAt = now; } }
      this.processingMessageIds.clear();
    }
    this.updateProcessingState(isProcessing, null, null);
  }

  private doProcess(message: StandardMessage, requestId?: string): boolean {
    if (requestId && !message.metadata?.requestId) {
      message = { ...message, metadata: { ...(message.metadata || {}), requestId } };
    }
    const msgRequestId = message.metadata?.requestId;
    if (msgRequestId && message.metadata?.isPlaceholder === true) this.requestMessageIdMap.set(msgRequestId, message.id);

    // 消息 ID 由生产者负责：
    // - 流式 LLM 输出：adapter.startStreamWithContext() → normalizer.startStream(boundId) 确保 ID = 占位符 ID
    // - 独立消息（INTERACTION、RESULT 等）：factory 创建时使用独立 ID
    // Pipeline 不做 ID 映射，只做状态管理、节流和事件分发

    this.validate(message, msgRequestId);
    if (message.category === MessageCategory.CONTENT) {
      message = this.ensureMessageEnvelope(message);
      this.updateStreamBufferFromMessage(message);
      message = this.ensureContentBlocksFromBuffer(message);
    } else {
      message = {
        ...message,
        eventId: message.eventId || this.generateEventId('evt'),
        eventSeq: this.resolveEventSeq(message.eventSeq),
      };
    }
    if (!this.validateCategory(message)) return false;

    if (!this.config.enabled) { this.recordRequestMessage(message); this.emitByCategory(message); return true; }

    const { id, lifecycle } = message;
    const now = Date.now();
    const existingState = this.messageStates.get(id);

    if (existingState && lifecycle === MessageLifecycle.STARTED) {
      if (existingState.completed) {
        // 新架构：每轮 tool calling 使用独立 messageId，不应出现同 ID 的二次 STARTED
        logger.warn('MessagePipeline.已完成_重复_START', { id, source: message.source, agent: message.agent }, LogCategory.SYSTEM);
        return false;
      }
      if (existingState.message === null) {
        existingState.message = message;
        existingState.lastSentAt = now;
        existingState.cardId = this.resolveCardId(id, message.metadata);
        if (typeof message.metadata?.cardStreamSeq === 'number') {
          existingState.lastCardStreamSeq = Math.max(existingState.lastCardStreamSeq, message.metadata.cardStreamSeq);
        }
        this.updateProcessingState(true, message.source, message.agent);
        this.recordRequestMessage(message); this.emitByCategory(message);
        return true;
      }
      // 占位消息 → 真实 LLM 消息转换：占位消息是 UI 桩，真实 STARTED 消息是 LLM 实际响应的起点
      // 允许真实消息替换占位消息，确保 requestId 等元数据正确传播到前端
      if (existingState.message?.metadata?.isPlaceholder) {
        existingState.message = message;
        existingState.lastSentAt = now;
        existingState.cardId = this.resolveCardId(id, message.metadata);
        if (typeof message.metadata?.cardStreamSeq === 'number') {
          existingState.lastCardStreamSeq = Math.max(existingState.lastCardStreamSeq, message.metadata.cardStreamSeq);
        }
        this.updateProcessingState(true, message.source, message.agent);
        this.recordRequestMessage(message); this.emitByCategory(message);
        return true;
      }
      logger.warn('MessagePipeline.重复_START', { id, source: message.source, agent: message.agent, lifecycle }, LogCategory.SYSTEM);
      return false;
    }

    if (lifecycle === MessageLifecycle.STARTED) {
      this.addActiveMessage(id, message, now);
      this.updateProcessingState(true, message.source, message.agent);
      this.recordRequestMessage(message); this.emitByCategory(message);
      return true;
    }

    if (!existingState) {
      this.addActiveMessage(id, message, now);
      if (lifecycle === MessageLifecycle.STREAMING) this.updateProcessingState(true, message.source, message.agent);
      this.recordRequestMessage(message); this.emitByCategory(message);
      if (this.isTerminalLifecycle(lifecycle)) {
        const finalSeq = message.metadata?.cardStreamSeq || 0;
        const completedMessage: StandardMessage = {
          ...message,
          metadata: {
            ...(message.metadata || {}),
            finalStreamSeq: finalSeq,
          },
        };
        this.markMessageComplete(id, completedMessage, now);
        this.safeEmit('unified:complete', completedMessage);
        this.checkAndUpdateProcessingState();
      }
      return true;
    }

    if (existingState.completed) {
      // 已完成的 state 收到新消息：允许终态消息更新内容（如 result 替换最终输出）
      if (this.isTerminalLifecycle(lifecycle)) {
        existingState.message = message;
        existingState.lastSentAt = now;
        const finalSeq = message.metadata?.cardStreamSeq || existingState.lastCardStreamSeq || 0;
        const completedMessage: StandardMessage = {
          ...message,
          metadata: {
            ...(message.metadata || {}),
            finalStreamSeq: finalSeq,
          },
        };
        this.recordRequestMessage(completedMessage); this.emitByCategory(completedMessage);
        return true;
      }
      // 非终态消息到达已完成 state：拒绝处理
      logger.warn('MessagePipeline.已完成_收到_非终态', { id, lifecycle, source: message.source, agent: message.agent }, LogCategory.SYSTEM);
      return false;
    }

    if (lifecycle === MessageLifecycle.STREAMING) {
      if (
        typeof message.metadata?.cardStreamSeq === 'number'
        && message.metadata.cardStreamSeq <= existingState.lastCardStreamSeq
      ) {
        logger.warn('MessagePipeline.乱序_STREAMING', {
          id,
          cardId: existingState.cardId,
          incomingCardStreamSeq: message.metadata.cardStreamSeq,
          lastCardStreamSeq: existingState.lastCardStreamSeq,
        }, LogCategory.SYSTEM);
        return false;
      }
      if (now - existingState.lastStreamAt < this.config.minStreamInterval) return false;
      existingState.lastStreamAt = now;
      existingState.message = message;
      existingState.cardId = this.resolveCardId(id, message.metadata);
      if (typeof message.metadata?.cardStreamSeq === 'number') {
        existingState.lastCardStreamSeq = Math.max(existingState.lastCardStreamSeq, message.metadata.cardStreamSeq);
      }
      this.recordRequestMessage(message); this.emitByCategory(message);
      return true;
    }

    if (this.isTerminalLifecycle(lifecycle)) {
      const finalSeq = message.metadata?.cardStreamSeq || existingState.lastCardStreamSeq || 0;
      const completedMessage: StandardMessage = {
        ...message,
        metadata: {
          ...(message.metadata || {}),
          finalStreamSeq: finalSeq,
        },
      };
      this.markMessageComplete(id, completedMessage, now);
      this.recordRequestMessage(completedMessage);
      this.emitByCategory(completedMessage);
      this.safeEmit('unified:complete', completedMessage);
      this.checkAndUpdateProcessingState();
      return true;
    }
    return true;
  }

  private validate(message: StandardMessage, requestId?: string): void {
    if (!message.id?.trim()) throw new Error('[MessagePipeline] StandardMessage missing id');
    if (Array.isArray(message.blocks)) {
      const invalid = message.blocks.filter(b => !b || typeof b !== 'object' || typeof (b as any).type !== 'string');
      if (invalid.length > 0) throw new Error(`[MessagePipeline] Invalid content blocks: ${message.id}`);
    }
    if (!message.source || !message.agent) throw new Error(`[MessagePipeline] StandardMessage missing source/agent: ${message.id}`);
    if (!message.category) throw new Error(`[MessagePipeline] StandardMessage missing category: ${message.id}`);
    if (message.category === MessageCategory.CONTENT && requestId !== undefined && (typeof requestId !== 'string' || !requestId.trim())) {
      logger.warn('MessagePipeline.内容消息缺少requestId', { id: message.id, source: message.source, agent: message.agent, lifecycle: message.lifecycle }, LogCategory.SYSTEM);
    }
  }

  private validateCategory(message: StandardMessage): boolean {
    switch (message.category) {
      case MessageCategory.CONTENT: {
        const isPlaceholder = message.metadata?.isPlaceholder === true;
        const isStreaming = message.lifecycle === MessageLifecycle.STARTED || message.lifecycle === MessageLifecycle.STREAMING;
        const isUserInput = message.type === MessageType.USER_INPUT;
        const hasBlocks = Array.isArray(message.blocks) && message.blocks.length > 0;
        if (!hasBlocks && !isPlaceholder && !isStreaming && !isUserInput) {
          logger.warn('MessagePipeline: Content message missing blocks, 跳过', { id: message.id, type: message.type, lifecycle: message.lifecycle }, LogCategory.SYSTEM);
          return false;
        }
        break;
      }
      case MessageCategory.CONTROL: if (!message.control) { logger.warn('MessagePipeline: Control message missing control payload, 跳过', { id: message.id }, LogCategory.SYSTEM); return false; } break;
      case MessageCategory.NOTIFY: if (!message.notify) { logger.warn('MessagePipeline: Notify message missing notify payload, 跳过', { id: message.id }, LogCategory.SYSTEM); return false; } break;
      case MessageCategory.DATA:
        if (!message.data) { logger.warn('MessagePipeline: Data message missing data payload, 跳过', { id: message.id }, LogCategory.SYSTEM); return false; }
        if (Array.isArray(message.blocks) && message.blocks.length > 0) { logger.warn('MessagePipeline: Data message must not carry blocks, 跳过', { id: message.id }, LogCategory.SYSTEM); return false; }
        break;
      default: logger.warn(`MessagePipeline: Unknown message category, 跳过`, { category: String(message.category), id: message.id }, LogCategory.SYSTEM); return false;
    }
    return true;
  }

  private safeEmit(event: string, data: unknown): boolean {
    try { return this.bus.emit(event, data); }
    catch (error) {
      logger.error('MessagePipeline.event_emit_failed', { event, error: error instanceof Error ? error.message : String(error), messageId: (data as StandardMessage)?.id }, LogCategory.SYSTEM);
      if (this.config.debug) throw error;
      return false;
    }
  }

  private emitByCategory(message: StandardMessage): void { this.safeEmit('unified:message', message); }

  private recordRequestMessage(message: StandardMessage): void {
    const requestId = message.metadata?.requestId;
    if (!requestId) return;
    const now = Date.now();
    const stats = this.requestMessageStats.get(requestId) || { totalContent: 0, assistantContent: 0, assistantThreadContent: 0, assistantWorkerContent: 0, assistantDispatchContent: 0, userContent: 0, placeholderContent: 0, dataCount: 0, messageIds: new Set<string>(), createdAt: now };
    if (stats.messageIds.has(message.id)) return;
    stats.messageIds.add(message.id);

    if (message.category === MessageCategory.DATA) { stats.dataCount += 1; this.requestMessageStats.set(requestId, stats); return; }
    if (message.category !== MessageCategory.CONTENT) { this.requestMessageStats.set(requestId, stats); return; }
    if (message.metadata?.isStatusMessage === true || message.type === MessageType.PROGRESS) { this.requestMessageStats.set(requestId, stats); return; }

    const hasText = Boolean(this.extractTextFromBlocks(message.blocks)) || Boolean(this.streamBuffers.get(message.id)?.text);
    const hasBlocks = this.hasRenderableBlocks(message.blocks) || this.hasRenderableBlocks(this.streamBuffers.get(message.id)?.lastBlocks);
    const isPlaceholder = message.metadata?.isPlaceholder === true;
    const isUserInput = message.type === MessageType.USER_INPUT;
    const isInstruction = message.type === MessageType.INSTRUCTION;

    if (!hasText && !hasBlocks && !isPlaceholder && !isUserInput) { this.requestMessageStats.set(requestId, stats); return; }

    stats.totalContent += 1;
    if (isPlaceholder) stats.placeholderContent += 1;
    else if (isUserInput) stats.userContent += 1;
    else if (message.source === 'worker') { stats.assistantWorkerContent += 1; stats.assistantContent += 1; }
    else if (isInstruction) stats.assistantDispatchContent += 1;
    else if (message.source === 'orchestrator') { stats.assistantThreadContent += 1; stats.assistantContent += 1; }
    else stats.assistantContent += 1;
    this.requestMessageStats.set(requestId, stats);
  }

  private toRequestSummary(stats: RequestMessageStats): RequestMessageSummary {
    return { totalContent: stats.totalContent, assistantContent: stats.assistantContent, assistantThreadContent: stats.assistantThreadContent, assistantWorkerContent: stats.assistantWorkerContent, assistantDispatchContent: stats.assistantDispatchContent, userContent: stats.userContent, placeholderContent: stats.placeholderContent, dataCount: stats.dataCount };
  }

  private isTerminalLifecycle(lifecycle: MessageLifecycle): boolean {
    return lifecycle === MessageLifecycle.COMPLETED || lifecycle === MessageLifecycle.FAILED || lifecycle === MessageLifecycle.CANCELLED;
  }

  private updateProcessingState(isProcessing: boolean, source: MessageSource | null, agent: string | null): void {
    const prev = this.processingState.isProcessing;
    this.processingState = { isProcessing, source: isProcessing ? source : null, agent: isProcessing ? agent : null, startedAt: isProcessing ? (this.processingState.startedAt || Date.now()) : null };
    if (prev !== isProcessing) this.safeEmit('processingStateChanged', this.getProcessingState());
  }

  private checkAndUpdateProcessingState(): void { if (!this.hasActiveMessages()) this.updateProcessingState(false, null, null); }

  private startCleanupTimer(): void {
    this.cleanupTimer = setInterval(() => {
      try {
        const now = Date.now();
        const expireTime = now - this.config.retentionTime;
        const msgToDelete: string[] = [], bufToDelete: string[] = [], reqToDelete: string[] = [];
        const cardToDelete = new Set<string>();
        for (const [id, state] of this.messageStates) { if (state.completed && state.lastSentAt < expireTime) msgToDelete.push(id); }
        for (const id of msgToDelete) {
          const state = this.messageStates.get(id);
          if (state?.cardId) {
            cardToDelete.add(state.cardId);
          }
          this.messageStates.delete(id);
        }
        for (const [id] of this.streamBuffers) { const state = this.messageStates.get(id); if (!state || state.completed) bufToDelete.push(id); }
        for (const id of bufToDelete) this.streamBuffers.delete(id);
        for (const [requestId, stats] of this.requestMessageStats) { if (stats.createdAt < expireTime) reqToDelete.push(requestId); }
        for (const requestId of reqToDelete) this.requestMessageStats.delete(requestId);
        for (const [cardId, sealed] of this.sealedCards) {
          if (sealed.sealedAt < expireTime) {
            cardToDelete.add(cardId);
            this.sealedCards.delete(cardId);
          }
        }
        for (const cardId of cardToDelete) {
          this.cardStreamSeqCounters.delete(cardId);
        }
        this.deadLetters = this.deadLetters.filter((entry) => entry.timestamp >= expireTime);
      } catch (error) { logger.error('MessagePipeline.cleanup_timer_failed', { error: error instanceof Error ? error.message : String(error) }, LogCategory.SYSTEM); }
    }, 60 * 1000);
  }

  private debugLog(action: string, messageId: string): void { if (this.config.debug) logger.debug('MessagePipeline.' + action, { messageId }, LogCategory.SYSTEM); }

  private extractTextFromBlocks(blocks?: ContentBlock[]): string {
    if (!Array.isArray(blocks) || blocks.length === 0) return '';
    return blocks.filter(b => b?.type === 'text' || b?.type === 'thinking').map(b => (b as any).content || '').filter(Boolean).join('\n');
  }

  private hasRenderableBlocks(blocks?: ContentBlock[]): boolean {
    if (!Array.isArray(blocks) || blocks.length === 0) return false;
    return blocks.some(b => {
      if (!b || typeof b !== 'object') return false;
      if (b.type === 'tool_call' || b.type === 'file_change' || b.type === 'plan') return true;
      return Boolean((b as any).content && String((b as any).content).trim());
    });
  }

  private updateStreamBufferFromMessage(message: StandardMessage): void {
    if (message.category !== MessageCategory.CONTENT) return;
    const buffer = this.streamBuffers.get(message.id) || { text: '' };
    if (Array.isArray(message.blocks) && message.blocks.length > 0) buffer.lastBlocks = message.blocks;
    const text = this.extractTextFromBlocks(message.blocks);
    if (text) buffer.text = text;
    this.streamBuffers.set(message.id, buffer);
  }

  private updateStreamBufferFromUpdate(update: StreamUpdate): void {
    const buffer = this.streamBuffers.get(update.messageId) || { text: '' };
    if (update.updateType === 'append' && update.appendText) buffer.text = `${buffer.text}${update.appendText}`;
    else if ((update.updateType === 'replace' || update.updateType === 'block_update') && update.blocks) { buffer.text = this.extractTextFromBlocks(update.blocks); buffer.lastBlocks = update.blocks; }
    this.streamBuffers.set(update.messageId, buffer);
  }

  private ensureContentBlocksFromBuffer(message: StandardMessage): StandardMessage {
    if (message.category !== MessageCategory.CONTENT || message.metadata?.isPlaceholder) return message;
    const hasBlocks = Array.isArray(message.blocks) && message.blocks.length > 0;
    const existingText = this.extractTextFromBlocks(message.blocks);
    if (hasBlocks && existingText?.trim()) return message;
    const buffer = this.streamBuffers.get(message.id);
    if (buffer?.lastBlocks?.length) return { ...message, blocks: buffer.lastBlocks, updatedAt: Date.now() };
    if (buffer?.text?.trim()) return { ...message, blocks: [{ type: 'text', content: buffer.text, isMarkdown: true }], updatedAt: Date.now() };
    return message;
  }

  dispose(): void {
    if (this.cleanupTimer) { clearInterval(this.cleanupTimer); this.cleanupTimer = null; }
    this.messageStates.clear();
    this.sealedCards.clear();
    this.cardStreamSeqCounters.clear();
    this.deadLetters = [];
    this.eventSeqCounter = 0;
    this.processingMessageIds.clear();
    this.requestMessageStats.clear();
    this.requestMessageIdMap.clear();
    this.streamBuffers.clear();
    this.processingState = { isProcessing: false, source: null, agent: null, startedAt: null };
  }
}
