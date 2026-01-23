/**
 * LLM Normalizer 基类
 * 
 * 职责：将各模型的原始输出转换为标准消息格式
 * 每个模型实现自己的 Normalizer，在适配层完成标准化
 */

import { logger, LogCategory } from '../logging';
import { EventEmitter } from 'events';
import type { AgentType } from '../types/agent-types';  // ✅ 使用 AgentType
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
import { parseContentToBlocks } from '../utils/content-parser';

/**
 * Normalizer 配置
 */
export interface NormalizerConfig {
  agent: AgentType;  // ✅ 使用 agent 替代旧字段
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
  hasAssistantText: boolean;
  pendingThinking: string | null;
  thinkingBlockId?: string;
  activeToolCalls: Map<string, ToolCallBlock>;
  interaction: InteractionRequest | null;
  startTime: number;
}

/**
 * LLM Normalizer 抽象基类
 */
export abstract class BaseNormalizer extends EventEmitter {
  protected config: NormalizerConfig;
  protected activeContexts: Map<string, ParseContext> = new Map();

  constructor(config: NormalizerConfig) {
    super();
    this.config = config;
  }

  get agent(): AgentType {  // ✅ 使用 agent 替代旧字段
    return this.config.agent;
  }

  startStream(traceId: string, source?: MessageSource): string {
    const messageId = generateMessageId();
    const context: ParseContext = {
      messageId,
      traceId,
      rawBuffer: '',
      blocks: [],
      pendingText: '',
      hasAssistantText: false,
      pendingThinking: null,
      thinkingBlockId: undefined,
      activeToolCalls: new Map(),
      interaction: null,
      startTime: Date.now(),
    };

    this.activeContexts.set(messageId, context);

    const message = createStreamingMessage(
      source || this.config.defaultSource,
      this.config.agent,  // ✅ 使用 agent
      traceId,
      { id: messageId }
    );

    this.emit('message', message);
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
      this.emit('update', update);
    }
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

    this.emit('complete', messageId, message);
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

    this.emit('complete', messageId, message);
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
    const blocks = [...context.blocks];

    if (context.pendingText.trim()) {
      // 🔍 检查点：记录 Worker 消息解析
      console.log('[DEBUG-WORKER] buildFinalMessage 解析 pendingText:', {
        messageId: context.messageId,
        pendingTextLength: context.pendingText.trim().length,
        pendingTextPreview: context.pendingText.trim().substring(0, 200),
      });

      const parsedBlocks = parseContentToBlocks(context.pendingText.trim());

      // 🔍 检查点：记录解析结果
      console.log('[DEBUG-WORKER] parseContentToBlocks 返回:', {
        messageId: context.messageId,
        blocksCount: parsedBlocks.length,
        blockTypes: parsedBlocks.map(b => b.type),
      });

      if (parsedBlocks.length > 0) {
        blocks.push(...parsedBlocks);
      }
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
    } else if (blocks.some(b => b.type === 'plan')) {
      // 🔧 新增：如果包含规划块，设置消息类型为 PLAN
      messageType = MessageType.PLAN;
    } else if (blocks.some(b => b.type === 'tool_call')) {
      messageType = MessageType.TOOL_CALL;
    }

    return createStandardMessage({
      id: context.messageId,
      traceId: context.traceId,
      type: messageType,
      source: this.config.defaultSource,
      agent: this.config.agent,  // ✅ 使用 agent
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

  protected addThinkingBlock(context: ParseContext, content: string, summary?: string, blockId?: string): void {
    if (content.trim()) {
      context.blocks.push({ type: 'thinking', content: content.trim(), summary, blockId } as ThinkingBlock);
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
