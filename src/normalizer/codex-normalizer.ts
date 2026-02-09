/**
 * Codex LLM Normalizer
 * 
 * 解析 Codex Agent 输出
 * 支持 --json 事件流与纯文本输出
 */

import {
  BaseNormalizer,
  ParseContext,
  NormalizerConfig,
} from './base-normalizer';
import {
  StreamUpdate,
  InteractionRequest,
  InteractionType,
  CodeBlock,
  generateMessageId,
} from '../protocol';
import { MESSAGE_EVENTS } from '../protocol/event-names';

/**
 * Codex LLM Normalizer
 */
export class CodexNormalizer extends BaseNormalizer {
  private codeBlockBuffer: string = '';
  private inCodeBlock: boolean = false;
  private codeBlockLang: string = '';
  private jsonBuffer: string = '';
  private static readonly THINKING_BLOCK_ID = 'codex-thinking';

  constructor(config?: Partial<NormalizerConfig>) {
    super({
      agent: 'codex',
      defaultSource: 'worker',
      ...config,
    });
  }

  // processTextDelta 使用 BaseNormalizer 默认实现（直接 append）
  // UniversalClient 已完成 SDK 事件解析，传入的是标准化纯文本 delta

  protected parseChunk(context: ParseContext, chunk: string): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    const plainLines: string[] = [];
    const lines = (this.jsonBuffer + chunk).split('\n');
    this.jsonBuffer = lines.pop() || '';

    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) {
        plainLines.push(line);
        continue;
      }
      try {
        const event = JSON.parse(trimmed) as Record<string, unknown>;
        const eventUpdates = this.processJsonEvent(context, event);
        updates.push(...eventUpdates);
      } catch {
        plainLines.push(line);
      }
    }

    if (plainLines.length > 0) {
      updates.push(...this.processTextChunk(context, plainLines.join('\n') + '\n'));
    }
    
    // 检测交互请求
    const interaction = this.detectInteraction(context, context.rawBuffer);
    if (interaction && !context.interaction) {
      context.interaction = interaction;
    }
    
    return updates;
  }

  protected finalizeContext(context: ParseContext): void {
    // 处理未完成的代码块
    if (this.inCodeBlock && this.codeBlockBuffer.trim()) {
      const codeBlock: CodeBlock = {
        type: 'code',
        language: this.codeBlockLang || 'text',
        content: this.codeBlockBuffer.trim(),
      };
      context.blocks.push(codeBlock);
    }
    this.inCodeBlock = false;
    this.codeBlockBuffer = '';
    this.codeBlockLang = '';
  }

  private processJsonEvent(context: ParseContext, event: Record<string, unknown>): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    const type = String(event.type || '');

    const text = this.extractText(event);
    if (text) {
      context.pendingText += text;
      context.hasAssistantText = true;
      updates.push(this.createUpdate(context.messageId, 'append', { appendText: text }));
    }

    const item = (event as { item?: Record<string, unknown> }).item;
    if (item) {
      const itemType = String(item.type || '');
      const itemText = this.extractText(item);
      if (itemType === 'reasoning' && itemText) {
        if (context.pendingThinking === null) {
          context.pendingThinking = '';
        }
        context.pendingThinking += itemText;
        updates.push(this.createUpdate(context.messageId, 'block_update', {
          blocks: [{
            type: 'thinking',
            content: context.pendingThinking,
            blockId: CodexNormalizer.THINKING_BLOCK_ID,
          }],
        }));
        return updates;
      }
      if (itemText) {
        context.pendingText += itemText;
        context.hasAssistantText = true;
        updates.push(this.createUpdate(context.messageId, 'append', { appendText: itemText }));
      }
    }

    // 处理 item delta 结构
    const delta = (event as { delta?: Record<string, unknown> }).delta;
    if (delta && typeof delta.text === 'string') {
      context.pendingText += delta.text;
      context.hasAssistantText = true;
      updates.push(this.createUpdate(context.messageId, 'append', { appendText: delta.text }));
    }

    return updates;
  }

  private processTextChunk(context: ParseContext, chunk: string): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    
    // 🔧 优化：直接流式传输所有文本，不再手动缓冲代码块
    // 前端 MarkdownContent 已具备强大的流式解析能力（包括自动补全未闭合代码块）
    // 直接传输可以让用户实时看到代码生成过程，而不是等待代码块结束
    
    context.pendingText += chunk;
    updates.push(this.createUpdate(context.messageId, 'append', { appendText: chunk }));
    
    return updates;
  }

  private extractText(value: unknown): string | undefined {
    if (!value || typeof value !== 'object') return undefined;
    const obj = value as Record<string, unknown>;
    if (typeof obj.text === 'string') return obj.text;
    if (typeof obj.content === 'string') return obj.content;
    if (typeof obj.message === 'string') return obj.message;
    if (obj.message && typeof obj.message === 'object') {
      const msg = obj.message as Record<string, unknown>;
      if (typeof msg.text === 'string') return msg.text;
      if (typeof msg.content === 'string') return msg.content;
    }
    return undefined;
  }

  protected detectInteraction(context: ParseContext, text: string): InteractionRequest | null {
    // Codex 的交互模式检测
    const patterns = [
      { regex: /\[y\/N\]/i, type: InteractionType.PERMISSION },
      { regex: /\[Y\/n\]/i, type: InteractionType.PERMISSION },
      { regex: /Press Enter to continue/i, type: InteractionType.CLARIFICATION },
      { regex: /Do you want to apply/i, type: InteractionType.PERMISSION },
    ];
    
    for (const { regex, type } of patterns) {
      if (regex.test(text)) {
        return {
          type,
          requestId: generateMessageId(),
          prompt: text.slice(-200), // 取最后 200 字符作为提示
          options: type === InteractionType.PERMISSION ? [
            { value: 'yes', label: '是', isDefault: true },
            { value: 'no', label: '否' },
          ] : undefined,
          required: type === InteractionType.PERMISSION,
        };
      }
    }
    
    return null;
  }
}
