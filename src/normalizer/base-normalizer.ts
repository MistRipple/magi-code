/**
 * CLI Normalizer 基类
 * 
 * 职责：将各 CLI 的原始输出转换为标准消息格式
 * 每个 CLI 实现自己的 Normalizer，在适配层完成标准化
 */

import { EventEmitter } from 'events';
import type { CLIType } from '../cli/types';
import {
  StandardMessage,
  StreamUpdate,
  ContentBlock,
  MessageType,
  MessageLifecycle,
  MessageSource,
  InteractionRequest,
  TextBlock,
  ThinkingBlock,
  ToolCallBlock,
  createStandardMessage,
  createStreamingMessage,
  generateMessageId,
} from '../protocol';

/**
 * Normalizer 配置
 */
export interface NormalizerConfig {
  cli: CLIType;
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
  rawBuffer: string;
  blocks: ContentBlock[];
  pendingText: string;
  pendingThinking: string | null;
  activeToolCalls: Map<string, ToolCallBlock>;
  interaction: InteractionRequest | null;
  startTime: number;
}

/**
 * CLI Normalizer 抽象基类
 */
export abstract class BaseNormalizer extends EventEmitter {
  protected config: NormalizerConfig;
  protected activeContexts: Map<string, ParseContext> = new Map();

  constructor(config: NormalizerConfig) {
    super();
    this.config = config;
  }

  get cli(): CLIType {
    return this.config.cli;
  }

  startStream(traceId: string, source?: MessageSource): string {
    const messageId = generateMessageId();
    const context: ParseContext = {
      messageId,
      traceId,
      rawBuffer: '',
      blocks: [],
      pendingText: '',
      pendingThinking: null,
      activeToolCalls: new Map(),
      interaction: null,
      startTime: Date.now(),
    };

    this.activeContexts.set(messageId, context);

    const message = createStreamingMessage(
      source || this.config.defaultSource,
      this.config.cli,
      traceId,
      { id: messageId }
    );

    this.emit('message', message);
    this.debug(`[${this.cli}] 开始流式消息: ${messageId}`);

    return messageId;
  }

  processChunk(messageId: string, chunk: string): void {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.cli}] 未找到消息上下文: ${messageId}`);
      return;
    }

    context.rawBuffer += chunk;
    const updates = this.parseChunk(context, chunk);

    for (const update of updates) {
      this.emit('update', update);
    }
  }

  endStream(messageId: string, error?: string): StandardMessage | null {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      this.debug(`[${this.cli}] 未找到消息上下文: ${messageId}`);
      return null;
    }

    this.finalizeContext(context);
    const message = this.buildFinalMessage(context, error);
    this.activeContexts.delete(messageId);

    this.emit('complete', messageId, message);
    this.debug(`[${this.cli}] 消息完成: ${messageId}, blocks: ${message.blocks.length}`);

    return message;
  }

  interruptStream(messageId: string): StandardMessage | null {
    const context = this.activeContexts.get(messageId);
    if (!context) {
      return null;
    }

    this.finalizeContext(context);
    const message = this.buildFinalMessage(context);
    message.lifecycle = MessageLifecycle.INTERRUPTED;
    this.activeContexts.delete(messageId);

    this.emit('complete', messageId, message);
    this.debug(`[${this.cli}] 消息中断: ${messageId}`);

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
    const blocks = [...context.blocks];

    if (context.pendingText.trim()) {
      blocks.push({ type: 'text', content: context.pendingText.trim(), isMarkdown: true } as TextBlock);
    }

    if (context.pendingThinking) {
      blocks.push({ type: 'thinking', content: context.pendingThinking } as ThinkingBlock);
    }

    for (const toolCall of context.activeToolCalls.values()) {
      blocks.push(toolCall);
    }

    let messageType = MessageType.TEXT;
    if (error) {
      messageType = MessageType.ERROR;
    } else if (context.interaction) {
      messageType = MessageType.INTERACTION;
    } else if (blocks.some(b => b.type === 'tool_call')) {
      messageType = MessageType.TOOL_CALL;
    }

    return createStandardMessage({
      id: context.messageId,
      traceId: context.traceId,
      type: messageType,
      source: this.config.defaultSource,
      cli: this.config.cli,
      lifecycle: error ? MessageLifecycle.FAILED : MessageLifecycle.COMPLETED,
      blocks,
      interaction: context.interaction || undefined,
      metadata: { duration: Date.now() - context.startTime, error },
    });
  }

  protected createUpdate(messageId: string, updateType: StreamUpdate['updateType'], data: Partial<StreamUpdate>): StreamUpdate {
    return { messageId, updateType, timestamp: Date.now(), ...data };
  }

  protected addTextBlock(context: ParseContext, text: string, isMarkdown = true): void {
    if (text.trim()) {
      context.blocks.push({ type: 'text', content: text.trim(), isMarkdown } as TextBlock);
    }
  }

  protected addThinkingBlock(context: ParseContext, content: string, summary?: string): void {
    if (content.trim()) {
      context.blocks.push({ type: 'thinking', content: content.trim(), summary } as ThinkingBlock);
    }
  }

  protected upsertToolCall(context: ParseContext, toolCall: ToolCallBlock): void {
    context.activeToolCalls.set(toolCall.toolId, toolCall);
  }

  protected completeToolCall(context: ParseContext, toolId: string, output?: string, error?: string): void {
    const toolCall = context.activeToolCalls.get(toolId);
    if (toolCall) {
      toolCall.status = error ? 'failed' : 'completed';
      toolCall.output = output;
      toolCall.error = error;
      context.blocks.push(toolCall);
      context.activeToolCalls.delete(toolId);
    }
  }

  protected debug(message: string, ...args: unknown[]): void {
    if (this.config.debug) {
      console.log(message, ...args);
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