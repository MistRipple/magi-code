/**
 * LLM Normalizer 基类
 * 
 * 职责：将各模型的原始输出转换为标准消息格式
 * 每个模型实现自己的 Normalizer，在适配层完成标准化
 */

import { logger, LogCategory } from '../logging';
import { EventEmitter } from 'events';
import type { AgentType } from '../types/agent-types';  // ✅ 使用 AgentType
import type { FileChangeMetadata } from '../llm/types';
import {
  StandardMessage,
  StreamUpdate,
  ContentBlock,
  MessageType,
  MessageLifecycle,
  MessageSource,
  MessageCategory,
  InteractionRequest,
  TextBlock,
  ThinkingBlock,
  ToolCallBlock,
  StandardizedToolResultPayload,
  createStandardMessage,
  createStreamingMessage,
  generateMessageId,
} from '../protocol';
import { parseContentToBlocks } from '../utils/content-parser';
import { MESSAGE_EVENTS } from '../protocol/event-names';

/**
 * Normalizer 配置
 */
export interface NormalizerConfig {
  agent: AgentType;
  defaultSource: MessageSource;
  debug?: boolean;
}

/**
 * Normalizer 事件
 */
export interface NormalizerEvents {
  message: (message: StandardMessage) => void;
  update: (update: StreamUpdate) => void;
  complete: (messageId: string, message: StandardMessage) => void;
  error: (error: Error, messageId?: string) => void;
}

/**
 * 解析上下文 - 用于跟踪流式解析状态
 */
export interface ParseContext {
  messageId: string;
  traceId: string;
  metadata: Record<string, unknown>;
  rawBuffer: string;
  blocks: ContentBlock[];
  pendingText: string;
  hasAssistantText: boolean;
  pendingThinking: string | null;
  thinkingBlockId?: string;
  thinkingBlockSeq: number;
  activeToolCalls: Map<string, ToolCallBlock>;
  interaction: InteractionRequest | null;
  durationStartAt: number;
  visibility?: 'user' | 'system' | 'debug';
}

function normalizePositiveTimestamp(value: unknown): number | undefined {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return undefined;
  }
  const normalized = Math.floor(value);
  return normalized > 0 ? normalized : undefined;
}

/**
 * LLM Normalizer 抽象基类
 */
export abstract class BaseNormalizer extends EventEmitter {
  private static readonly USER_HIDDEN_TOOL_NAMES = new Set<string>([
    'context_compact',
  ]);
  protected config: NormalizerConfig;
  protected activeContexts: Map<string, ParseContext> = new Map();

  constructor(config: NormalizerConfig) {
    super();
    this.config = config;
  }

  get agent(): AgentType {
    return this.config.agent;
  }

  private buildCanonicalMetadata(metadata: Record<string, unknown> | undefined, durationStartAt: number): Record<string, unknown> {
    return {
      ...(metadata ? { ...metadata } : {}),
      // 时间轴锚点必须在 stream 创建时一次性确定。
      // 后续 UPDATE / COMPLETE 只能复用，禁止再次发明另一套排序事实。
      timelineAnchorTimestamp: normalizePositiveTimestamp(metadata?.timelineAnchorTimestamp) || durationStartAt,
    };
  }

  private buildFinalMetadata(context: ParseContext, error?: string): Record<string, unknown> {
    return {
      ...context.metadata,
      duration: Date.now() - context.durationStartAt,
      error,
    };
  }

  startStream(traceId: string, source?: MessageSource, messageIdOverride?: string, visibility?: 'user' | 'system' | 'debug', metadata?: Record<string, unknown>): string {
    const normalizedId = typeof messageIdOverride === 'string' && messageIdOverride.trim()
      ? messageIdOverride.trim()
      : undefined;
    const messageId = normalizedId || generateMessageId();
    if (this.activeContexts.has(messageId)) {
      throw new Error(`[${this.agent}] Stream messageId already active: ${messageId}`);
    }
    const durationStartAt = Date.now();
    const canonicalMetadata = this.buildCanonicalMetadata(metadata, durationStartAt);
    const context: ParseContext = {
      messageId,
      traceId,
      metadata: canonicalMetadata,
      rawBuffer: '',
      blocks: [],
      pendingText: '',
      hasAssistantText: false,
      pendingThinking: null,
      thinkingBlockId: undefined,
      thinkingBlockSeq: 0,
      activeToolCalls: new Map(),
      interaction: null,
      durationStartAt,
      visibility,
    };

    this.activeContexts.set(messageId, context);

    const message = createStreamingMessage(
      source || this.config.defaultSource,
      this.config.agent,
      traceId,
      {
        id: messageId,
        visibility: visibility || 'user',
        // started message 必须携带 canonical metadata，
        // 让 live / restore / complete 共用同一事件事实源。
        metadata: canonicalMetadata,
      }
    );

    this.emit(MESSAGE_EVENTS.MESSAGE, message);
    this.debug(`[${this.agent}] 开始流式消息: ${messageId}`);  // ✅ 使用 agent

    return messageId;
  }

  processChunk(messageId: string, chunk: string): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] 未找到消息上下文: ${messageId}`);  // ✅ 使用 agent
      return;
    }

    context.rawBuffer += chunk;
    const updates = this.parseChunk(context, chunk);

    for (const update of updates) {
      this.emit(MESSAGE_EVENTS.UPDATE, update);
    }
  }

  /**
   * 处理已经标准化的文本增量（不走 JSON 解析）
   *
   * 适用于 LLM 客户端已输出结构化 delta 的场景，避免依赖行分隔 JSON。
   */
  processTextDelta(messageId: string, delta: string): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] 未找到消息上下文: ${messageId}`);  // ✅ 使用 agent
      return;
    }
    if (!delta) return;
    this.flushPendingThinkingToBlocks(context, 'processTextDelta');
    context.pendingText += delta;
    context.hasAssistantText = true;
    const update = this.createUpdate(messageId, 'append', { appendText: delta });
    this.emit(MESSAGE_EVENTS.UPDATE, update);
  }

  /**
   * 处理 thinking 内容（用于流式 thinking 输出）
   */
  processThinking(messageId: string, thinkingContent: string): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] 未找到消息上下文: ${messageId}`);
      return;
    }

    // 累积 thinking 内容
    if (context.pendingThinking === null) {
      context.pendingThinking = '';
    }
    context.pendingThinking += thinkingContent;

    // 生成 thinking block ID（如果还没有）
    if (!context.thinkingBlockId) {
      context.thinkingBlockId = this.allocateThinkingBlockId(context);
    }

    // 发送 thinking 更新
    const update = this.createUpdate(messageId, 'block_update', {
      blocks: [{
        type: 'thinking',
        content: context.pendingThinking,
        blockId: context.thinkingBlockId,
      }],
    });
    this.emit(MESSAGE_EVENTS.UPDATE, update);
  }

  /**
   * 处理 Token 使用统计
   */
  processUsage(messageId: string, usage: {
    inputTokens?: number;
    outputTokens?: number;
    cacheReadTokens?: number;
    cacheWriteTokens?: number;
  }): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] 未找到消息上下文: ${messageId}`);
      return;
    }

    const update = this.createUpdate(messageId, 'block_update', {
      tokenUsage: usage
    });
    this.emit(MESSAGE_EVENTS.UPDATE, update);
  }

  endStream(messageId: string, error?: string): StandardMessage | null {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] 未找到消息上下文: ${messageId}`);  // ✅ 使用 agent
      return null;
    }

    this.finalizeContext(context);
    const message = this.buildFinalMessage(context, error);
    this.activeContexts.delete(messageId);

    this.emit(MESSAGE_EVENTS.COMPLETE, messageId, message);
    this.debug(`[${this.agent}] 消息完成: ${messageId}, blocks: ${message.blocks.length}`);  // ✅ 使用 agent

    return message;
  }

  interruptStream(messageId: string): StandardMessage | null {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      return null;
    }

    this.finalizeContext(context);
    const message = this.buildFinalMessage(context);
    message.lifecycle = MessageLifecycle.CANCELLED;
    this.activeContexts.delete(messageId);

    this.emit(MESSAGE_EVENTS.COMPLETE, messageId, message);
    this.debug(`[${this.agent}] 消息中断: ${messageId}`);  // ✅ 使用 agent

    return message;
  }

  getActiveMessageIds(): string[] {
    return Array.from(this.activeContexts.keys());
  }

  hasActiveStream(): boolean {
    return this.activeContexts.size > 0;
  }

  // 抽象方法 - 子类必须实现
  protected abstract parseChunk(context: ParseContext, chunk: string): StreamUpdate[];
  protected abstract finalizeContext(context: ParseContext): void;
  protected abstract detectInteraction(context: ParseContext, text: string): InteractionRequest | null;

  // 辅助方法
  protected buildFinalMessage(context: ParseContext, error?: string): StandardMessage {
    this.flushPendingTextToBlocks(context);
    this.flushPendingThinkingToBlocks(context, 'buildFinalMessage');
    const blocks = this.sanitizeBlocks([...context.blocks], 'buildFinalMessage');

    for (const toolCall of context.activeToolCalls.values()) {
      if (!this.isUserVisibleToolBlock(toolCall, context.visibility)) {
        continue;
      }
      blocks.push(toolCall);
    }

    let messageType = MessageType.TEXT;
    if (error) {
      messageType = MessageType.ERROR;
    } else if (context.interaction) {
      messageType = MessageType.INTERACTION;
    } else if (blocks.some(b => b.type === 'plan')) {
      // 🔧 新增：如果包含规划块，设置消息类型为 PLAN
      messageType = MessageType.PLAN;
    } else if (blocks.some(b => b.type === 'tool_call')) {
      messageType = MessageType.TOOL_CALL;
    } else if (blocks.some(b => b.type === 'thinking')) {
      // 🔧 方案 B 修复：如果包含思考块，设置消息类型为 THINKING
      messageType = MessageType.THINKING;
    }

    const safeBlocks = this.sanitizeBlocks(blocks, 'buildFinalMessage.final');
    return createStandardMessage({
      id: context.messageId,
      traceId: context.traceId,
      category: MessageCategory.CONTENT,  // 🔧 统一消息通道：LLM 输出为 CONTENT 类别
      type: messageType,
      source: this.config.defaultSource,
      agent: this.config.agent,  // ✅ 使用 agent
      lifecycle: error ? MessageLifecycle.FAILED : MessageLifecycle.COMPLETED,
      blocks: safeBlocks,
      interaction: context.interaction || undefined,
      visibility: context.visibility || 'user',
      metadata: this.buildFinalMetadata(context, error),
    });
  }

  protected sanitizeBlocks(blocks: ContentBlock[], context: string): ContentBlock[] {
    const invalid = (blocks || []).filter(
      (block) => !block || typeof block !== 'object' || typeof (block as ContentBlock).type !== 'string'
    );
    if (invalid.length > 0) {
      logger.error('规范化.块_无效', {
        agent: this.config.agent,
        context,
        invalidCount: invalid.length,
      }, LogCategory.SYSTEM);
      throw new Error(`Invalid content blocks in ${context}`);
    }
    return blocks || [];
  }

  protected createUpdate(messageId: string, updateType: StreamUpdate['updateType'], data: Partial<StreamUpdate>): StreamUpdate {
    return { messageId, updateType, timestamp: Date.now(), ...data };
  }

  protected addTextBlock(context: ParseContext, text: string, isMarkdown = true): void {
    if (text.trim()) {
      context.blocks.push({ type: 'text', content: text.trim(), isMarkdown } as TextBlock);
    }
  }

  protected flushPendingTextToBlocks(context: ParseContext, reason = 'flushPendingTextToBlocks'): void {
    if (!context.pendingText.trim()) {
      return;
    }

    const parsedBlocks = parseContentToBlocks(context.pendingText.trim());
    if (parsedBlocks.length > 0) {
      context.blocks.push(...this.sanitizeBlocks(parsedBlocks, `${reason}.pendingText`));
    }
    context.pendingText = '';
  }

  protected allocateThinkingBlockId(context: ParseContext): string {
    context.thinkingBlockSeq += 1;
    return `${context.messageId}-thinking-${context.thinkingBlockSeq}`;
  }

  protected flushPendingThinkingToBlocks(context: ParseContext, reason = 'flushPendingThinkingToBlocks'): void {
    if (!context.pendingThinking?.trim()) {
      return;
    }

    this.addThinkingBlock(context, context.pendingThinking, undefined, context.thinkingBlockId);
    context.pendingThinking = null;
    context.thinkingBlockId = undefined;
    this.debug(`[${this.agent}] thinking 段已固化: ${reason}`);
  }

  protected addThinkingBlock(context: ParseContext, content: string, summary?: string, blockId?: string): void {
    if (content.trim()) {
      context.blocks.push({ type: 'thinking', content: content.trim(), summary, blockId } as ThinkingBlock);
    }
  }

  protected upsertToolCall(context: ParseContext, toolCall: ToolCallBlock): void {
    // 保持最终消息与流式 UI 一致：工具调用开始前，先把已有文本正式落块，
    // 避免 buildFinalMessage 时按完成时机把工具卡“挪到底部”。
    this.flushPendingThinkingToBlocks(context, 'upsertToolCall');
    this.flushPendingTextToBlocks(context, 'upsertToolCall');
    context.activeToolCalls.set(toolCall.toolId, toolCall);
  }

  /**
   * 添加工具调用到消息上下文（public 接口）
   * 用于外部模块（如 worker-adapter）在获取到工具调用信息后同步到 Normalizer
   */
  public addToolCall(messageId: string, toolCall: ToolCallBlock): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] addToolCall: 未找到消息上下文: ${messageId}`);
      return;
    }
    this.upsertToolCall(context, toolCall);
    if (!this.isUserVisibleToolBlock(toolCall, context.visibility)) {
      return;
    }
    // 发送更新通知
    const update = this.createUpdate(messageId, 'block_update', { blocks: [toolCall] });
    this.emit(MESSAGE_EVENTS.UPDATE, update);
  }

  /**
   * 完成工具调用（public 接口）
   * 用于外部模块在工具执行完成后更新状态
   * 当 fileChange 存在且无错误时，自动生成 file_change block 供前端差异化面板
   */
  public finishToolCall(
    messageId: string,
    toolId: string,
    output?: string,
    error?: string,
    fileChange?: FileChangeMetadata,
    standardized?: StandardizedToolResultPayload,
  ): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] finishToolCall: 未找到消息上下文: ${messageId}`);
      return;
    }

    // 在 completeToolCall 将工具从 activeToolCalls 移到 blocks 之前，获取原始信息
    const originalTool = context.activeToolCalls.get(toolId);
    const toolName = originalTool?.toolName || '';
    const input = originalTool?.input;

    const normalizedOutcome = this.normalizeToolCompletionOutcome(output, error, standardized);
    const isUserVisibleTool = this.isUserVisibleToolName(toolName, context.visibility);
    this.completeToolCall(
      context,
      toolId,
      normalizedOutcome.output,
      normalizedOutcome.error,
      standardized,
      isUserVisibleTool,
    );

    // 发送 UPDATE 事件，通知前端工具执行完成状态（保留原始 toolName 和 input，避免 mergeBlocks 覆盖）
    if (!isUserVisibleTool) {
      return;
    }
    const update = this.createUpdate(messageId, 'block_update', {
      blocks: [{
        type: 'tool_call',
        toolName,
        toolId,
        status: normalizedOutcome.status,
        input,
        output: normalizedOutcome.output,
        error: normalizedOutcome.error,
        standardized,
      }],
    });
    this.emit(MESSAGE_EVENTS.UPDATE, update);

    // 文件变更工具执行成功后，自动附加 file_change block 供前端展示差异化面板
    if (!error && fileChange) {
      this.addFileChangeBlock(messageId, fileChange.filePath, fileChange.changeType, fileChange.additions, fileChange.deletions, fileChange.diff);
    }
  }

  public settleToolCallBlock(
    messageId: string,
    toolCall: ToolCallBlock,
    fileChange?: FileChangeMetadata,
  ): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] settleToolCallBlock: 未找到消息上下文: ${messageId}`);
      return;
    }

    const originalTool = context.activeToolCalls.get(toolCall.toolId);
    const finalizedToolCall: ToolCallBlock = {
      ...(originalTool || {
        type: 'tool_call',
        toolName: toolCall.toolName,
        toolId: toolCall.toolId,
        status: toolCall.status,
      }),
      ...toolCall,
      input: toolCall.input ?? originalTool?.input,
      output: toolCall.output ?? originalTool?.output,
      error: toolCall.error ?? originalTool?.error,
      standardized: toolCall.standardized ?? originalTool?.standardized,
    };
    const isUserVisibleTool = this.isUserVisibleToolBlock(finalizedToolCall, context.visibility);

    if (isUserVisibleTool) {
      context.blocks.push(finalizedToolCall);
      const update = this.createUpdate(messageId, 'block_update', {
        blocks: [finalizedToolCall],
      });
      this.emit(MESSAGE_EVENTS.UPDATE, update);
    }

    context.activeToolCalls.delete(toolCall.toolId);

    if (!finalizedToolCall.error && fileChange) {
      this.addFileChangeBlock(messageId, fileChange.filePath, fileChange.changeType, fileChange.additions, fileChange.deletions, fileChange.diff);
    }
  }

  /**
   * 附加文件变更块（public 接口）
   * 用于文件写工具（file_edit/file_insert/file_create）执行成功后，在对话流中展示差异化面板
   */
  public addFileChangeBlock(
    messageId: string,
    filePath: string,
    changeType: 'create' | 'modify' | 'delete',
    additions?: number,
    deletions?: number,
    diff?: string,
  ): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.agent}] addFileChangeBlock: 未找到消息上下文: ${messageId}`);
      return;
    }

    const block: ContentBlock = {
      type: 'file_change',
      filePath,
      changeType,
      additions,
      deletions,
      diff,
    } as ContentBlock;

    this.flushPendingThinkingToBlocks(context, 'addFileChangeBlock');
    this.flushPendingTextToBlocks(context, 'addFileChangeBlock');
    context.blocks.push(block);

    const update = this.createUpdate(messageId, 'block_update', { blocks: [block] });
    this.emit(MESSAGE_EVENTS.UPDATE, update);
  }

  protected completeToolCall(
    context: ParseContext,
    toolId: string,
    output?: string,
    error?: string,
    standardized?: StandardizedToolResultPayload,
    shouldExposeToUser = true,
  ): void {
    const toolCall = context.activeToolCalls.get(toolId);
    if (toolCall) {
      const normalizedOutcome = this.normalizeToolCompletionOutcome(output, error, standardized);
      toolCall.status = normalizedOutcome.status;
      toolCall.output = normalizedOutcome.output;
      toolCall.error = normalizedOutcome.error;
      toolCall.standardized = standardized;
      if (shouldExposeToUser) {
        context.blocks.push(toolCall);
      }
      context.activeToolCalls.delete(toolId);
    }
  }

  protected isUserVisibleToolName(
    toolName: string,
    visibility?: 'user' | 'system' | 'debug',
  ): boolean {
    if (visibility === 'debug') {
      return true;
    }
    if (visibility === 'system') {
      return false;
    }
    return !BaseNormalizer.USER_HIDDEN_TOOL_NAMES.has((toolName || '').trim());
  }

  protected isUserVisibleToolBlock(
    toolCall: ToolCallBlock,
    visibility?: 'user' | 'system' | 'debug',
  ): boolean {
    return this.isUserVisibleToolName(toolCall.toolName, visibility);
  }

  private normalizeToolCompletionOutcome(
    output?: string,
    error?: string,
    standardized?: StandardizedToolResultPayload,
  ): { status: 'completed' | 'failed'; output?: string; error?: string } {
    const status = (standardized?.status || '').toLowerCase();
    const hardFailure = status === 'error' || status === 'timeout' || status === 'killed';
    if (hardFailure) {
      return {
        status: 'failed',
        output: undefined,
        error: standardized?.message || error || output || 'Tool execution failed',
      };
    }

    if (error) {
      return { status: 'failed', output: undefined, error };
    }

    return { status: 'completed', output, error: undefined };
  }

  protected debug(message: string, ...args: unknown[]): void {
    if (this.config.debug) {
      logger.debug('规范化.调试', { message, args }, LogCategory.SYSTEM);
    }
  }

  on<K extends keyof NormalizerEvents>(event: K, listener: NormalizerEvents[K]): this {
    return super.on(event, listener);
  }

  off<K extends keyof NormalizerEvents>(event: K, listener: NormalizerEvents[K]): this {
    return super.off(event, listener);
  }

  emit<K extends keyof NormalizerEvents>(event: K, ...args: Parameters<NormalizerEvents[K]>): boolean {
    return super.emit(event, ...args);
  }
}
