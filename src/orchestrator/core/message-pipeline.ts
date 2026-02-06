/**
 * MessagePipeline - 消息协议层
 *
 * 负责消息校验、去重、节流、生命周期管理
 * 接收 MessageBus 作为构造参数，验证通过的消息会发射到 Bus
 */

import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import type { StandardMessage, ContentBlock, StreamUpdate, MessageSource } from '../../protocol/message-protocol';
import { MessageType, MessageLifecycle, MessageCategory } from '../../protocol/message-protocol';

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

  private addActiveMessage(id: string, message: StandardMessage, ts: number): void {
    this.messageStates.set(id, { message, createdAt: ts, lastSentAt: ts, lastStreamAt: ts, completed: false });
  }

  private markMessageComplete(id: string, message: StandardMessage, ts: number): void {
    const state = this.messageStates.get(id);
    if (state) { state.message = message; state.lastSentAt = ts; state.completed = true; }
    this.streamBuffers.delete(id);
  }

  private hasActiveMessages(): boolean {
    for (const state of this.messageStates.values()) { if (!state.completed) return true; }
    return false;
  }

  clearMessageState(messageId: string): void { this.messageStates.delete(messageId); }

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
    if (!this.config.enabled) { this.updateStreamBufferFromUpdate(update); this.safeEmit('unified:update', update); return true; }
    const now = Date.now();
    this.updateStreamBufferFromUpdate(update);
    let state = this.messageStates.get(update.messageId);
    if (!state) { state = { message: null, createdAt: now, lastSentAt: 0, lastStreamAt: 0, completed: false }; this.messageStates.set(update.messageId, state); }
    if (state.completed) { logger.warn('MessagePipeline.完成后更新', { messageId: update.messageId }, LogCategory.SYSTEM); return false; }
    if (now - state.lastStreamAt < this.config.minStreamInterval) return false;
    state.lastStreamAt = now;
    this.safeEmit('unified:update', update);
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

    const isPlaceholder = message.metadata?.isPlaceholder === true;
    const isUserInput = message.type === MessageType.USER_INPUT;
    const isStatusMsg = message.metadata?.isStatusMessage === true;
    const isTaskCard = message.type === MessageType.TASK_CARD;
    const isInstruction = message.type === MessageType.INSTRUCTION;

    if (message.category === MessageCategory.CONTENT && message.source === 'orchestrator' && msgRequestId && !isPlaceholder && !isUserInput && !isStatusMsg && !isTaskCard && !isInstruction) {
      const boundId = this.getRequestMessageId(msgRequestId);
      if (!boundId) throw new Error(`[MessagePipeline] 主响应消息缺少占位绑定: requestId=${msgRequestId}`);
      if (message.id !== boundId) message = { ...message, id: boundId };
    }

    this.validate(message, msgRequestId);
    if (message.category === MessageCategory.CONTENT) { this.updateStreamBufferFromMessage(message); message = this.ensureContentBlocksFromBuffer(message); }
    this.validateCategory(message);

    if (!this.config.enabled) { this.recordRequestMessage(message); this.emitByCategory(message); return true; }

    const { id, lifecycle } = message;
    const now = Date.now();
    const existingState = this.messageStates.get(id);

    if (existingState && lifecycle === MessageLifecycle.STARTED) {
      if (existingState.message === null) {
        existingState.message = message; existingState.lastSentAt = now;
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
      if (this.isTerminalLifecycle(lifecycle)) { this.markMessageComplete(id, message, now); this.safeEmit('unified:complete', message); this.checkAndUpdateProcessingState(); }
      return true;
    }

    if (existingState.completed) { logger.warn('MessagePipeline.重复_完成', { id, source: message.source, agent: message.agent, lifecycle }, LogCategory.SYSTEM); return false; }

    if (lifecycle === MessageLifecycle.STREAMING) {
      if (now - existingState.lastStreamAt < this.config.minStreamInterval) return false;
      existingState.lastStreamAt = now; existingState.message = message;
      this.recordRequestMessage(message); this.emitByCategory(message);
      return true;
    }

    if (this.isTerminalLifecycle(lifecycle)) {
      this.markMessageComplete(id, message, now);
      this.recordRequestMessage(message); this.emitByCategory(message);
      this.safeEmit('unified:complete', message); this.checkAndUpdateProcessingState();
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

  private validateCategory(message: StandardMessage): void {
    switch (message.category) {
      case MessageCategory.CONTENT: {
        const isPlaceholder = message.metadata?.isPlaceholder === true;
        const isStreaming = message.lifecycle === MessageLifecycle.STARTED || message.lifecycle === MessageLifecycle.STREAMING;
        const isUserInput = message.type === MessageType.USER_INPUT;
        const hasBlocks = Array.isArray(message.blocks) && message.blocks.length > 0;
        if (!hasBlocks && !isPlaceholder && !isStreaming && !isUserInput) throw new Error(`[MessagePipeline] Content message missing blocks: ${message.id}`);
        break;
      }
      case MessageCategory.CONTROL: if (!message.control) throw new Error(`[MessagePipeline] Control message missing control payload: ${message.id}`); break;
      case MessageCategory.NOTIFY: if (!message.notify) throw new Error(`[MessagePipeline] Notify message missing notify payload: ${message.id}`); break;
      case MessageCategory.DATA:
        if (!message.data) throw new Error(`[MessagePipeline] Data message missing data payload: ${message.id}`);
        if (Array.isArray(message.blocks) && message.blocks.length > 0) throw new Error(`[MessagePipeline] Data message must not carry blocks: ${message.id}`);
        break;
      default: throw new Error(`[MessagePipeline] Unknown message category: ${String(message.category)} (${message.id})`);
    }
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
        for (const [id, state] of this.messageStates) { if (state.completed && state.lastSentAt < expireTime) msgToDelete.push(id); }
        for (const id of msgToDelete) this.messageStates.delete(id);
        for (const [id] of this.streamBuffers) { const state = this.messageStates.get(id); if (!state || state.completed) bufToDelete.push(id); }
        for (const id of bufToDelete) this.streamBuffers.delete(id);
        for (const [requestId, stats] of this.requestMessageStats) { if (stats.createdAt < expireTime) reqToDelete.push(requestId); }
        for (const requestId of reqToDelete) this.requestMessageStats.delete(requestId);
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
    this.processingMessageIds.clear();
    this.requestMessageStats.clear();
    this.requestMessageIdMap.clear();
    this.streamBuffers.clear();
    this.processingState = { isProcessing: false, source: null, agent: null, startedAt: null };
  }
}
