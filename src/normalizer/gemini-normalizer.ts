/**
 * Gemini LLM Normalizer
 * 
 * 解析 Gemini LLM 的混合格式输出
 * Gemini 支持流式输出，格式介于 Claude 和 Codex 之间
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
  ToolCallBlock,
  generateMessageId,
} from '../protocol';
import { MESSAGE_EVENTS } from '../protocol/event-names';

/**
 * Gemini LLM Normalizer
 */
export class GeminiNormalizer extends BaseNormalizer {
  private jsonBuffer: string = '';
  private static readonly THINKING_BLOCK_ID = 'gemini-thinking';

  constructor(config?: Partial<NormalizerConfig>) {
    super({
      agent: 'gemini',
      defaultSource: 'worker',
      ...config,
    });
  }

  // processTextDelta 使用 BaseNormalizer 默认实现（直接 append）
  // UniversalClient 已完成 SDK 事件解析，传入的是标准化纯文本 delta

  protected parseChunk(context: ParseContext, chunk: string): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    
    // Gemini 可能输出 JSON 或纯文本
    // 先尝试检测是否为 JSON 格式
    const trimmedChunk = chunk.trim();
    
    if (trimmedChunk.startsWith('{') || this.jsonBuffer) {
      // 可能是 JSON 格式
      this.jsonBuffer += chunk;
      
      // 尝试解析完整的 JSON
      try {
        const data = JSON.parse(this.jsonBuffer);
        const jsonUpdates = this.processJsonData(context, data);
        updates.push(...jsonUpdates);
        this.jsonBuffer = '';
      } catch {
        // JSON 不完整，继续累积
      }
    } else {
      // 纯文本处理
      const textUpdates = this.processTextChunk(context, chunk);
      updates.push(...textUpdates);
    }
    
    // 检测交互请求
    const interaction = this.detectInteraction(context, context.rawBuffer);
    if (interaction && !context.interaction) {
      context.interaction = interaction;
    }
    
    return updates;
  }

  private processJsonData(context: ParseContext, data: Record<string, unknown>): StreamUpdate[] {
    const updates: StreamUpdate[] = [];

    // 处理 Gemini 的 JSON 响应格式
    if (data.type === 'text' && typeof data.content === 'string') {
      context.pendingText += data.content;
      updates.push(this.createUpdate(context.messageId, 'append', { appendText: data.content }));
    } else if (data.type === 'thinking' || data.type === 'reasoning') {
      // 处理 thinking/reasoning 内容
      const thinkingContent = (data.content as string) || (data.text as string) || '';
      if (thinkingContent) {
        if (context.pendingThinking === null) {
          context.pendingThinking = '';
        }
        context.pendingThinking += thinkingContent;
        updates.push(this.createUpdate(context.messageId, 'block_update', {
          blocks: [{
            type: 'thinking',
            content: context.pendingThinking,
            blockId: GeminiNormalizer.THINKING_BLOCK_ID,
          }],
        }));
      }
    } else if (data.type === 'tool_call' || data.type === 'function_call') {
      // 后端统一序列化 input 为 JSON 字符串
      const rawInput = data.args || data.input;
      const toolCall: ToolCallBlock = {
        type: 'tool_call',
        toolName: (data.name as string) || 'unknown',
        toolId: (data.id as string) || generateMessageId(),
        status: 'running',
        input: rawInput ? JSON.stringify(rawInput, null, 2) : undefined,
      };
      this.upsertToolCall(context, toolCall);
      updates.push(this.createUpdate(context.messageId, 'block_update', { blocks: [toolCall] }));
    } else if (data.type === 'tool_result' || data.type === 'function_result') {
      const toolId = this.findActiveToolId(context);
      if (toolId) {
        const output = (data.result as string) || (data.output as string) || '';
        const error = data.error ? String(data.error) : undefined;
        this.completeToolCall(context, toolId, error ? undefined : output, error);
      }
    }
    
    return updates;
  }

  private processTextChunk(context: ParseContext, chunk: string): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    
    // 🔧 优化：直接流式传输所有文本，不再手动缓冲代码块
    // 前端已具备强大的流式 Markdown 解析能力（包括自动补全未闭合代码块），
    // 直接传输可以让用户实时看到代码生成过程，而不是等待代码块结束。
    
    context.pendingText += chunk;
    updates.push(this.createUpdate(context.messageId, 'append', { appendText: chunk }));
    
    return updates;
  }

  protected finalizeContext(context: ParseContext): void {
    // 处理未完成的 JSON
    if (this.jsonBuffer.trim()) {
      context.pendingText += this.jsonBuffer;
    }
    this.jsonBuffer = '';
  }

  protected detectInteraction(context: ParseContext, text: string): InteractionRequest | null {
    // Gemini 的交互模式检测
    const patterns = [
      { regex: /\[y\/n\]/i, type: InteractionType.PERMISSION },
      { regex: /confirm|确认/i, type: InteractionType.CLARIFICATION },
      { regex: /would you like|是否/i, type: InteractionType.QUESTION },
    ];
    
    for (const { regex, type } of patterns) {
      if (regex.test(text)) {
        return {
          type,
          requestId: generateMessageId(),
          prompt: text.slice(-200),
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

  private findActiveToolId(context: ParseContext): string | null {
    const toolIds = Array.from(context.activeToolCalls.keys());
    return toolIds.length > 0 ? toolIds[toolIds.length - 1] : null;
  }
}
