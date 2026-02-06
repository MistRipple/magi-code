/**
 * Claude LLM Normalizer
 * 
 * 解析 Claude LLM 的 stream-json 格式输出
 * 将其转换为标准消息格式
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
  ToolCallBlock,
  generateMessageId,
} from '../protocol';
import { MESSAGE_EVENTS } from '../protocol/event-names';

/**
 * Claude stream-json 事件类型
 */
interface ClaudeStreamEvent {
  type: string;
  subtype?: string;
  message?: {
    id?: string;
    content?: Array<{
      type: string;
      text?: string;
      id?: string;
      name?: string;
      input?: Record<string, unknown>;
    }>;
  };
  index?: number;
  content_block?: {
    type: string;
    text?: string;
    id?: string;
    name?: string;
    input?: Record<string, unknown>;
  };
  delta?: {
    type: string;
    text?: string;
    partial_json?: string;
    thinking?: string;
  };
  result?:
    | {
        type: string;
        content?: string;
        output?: string;
        is_error?: boolean;
      }
    | string;
}

/**
 * Claude LLM Normalizer
 */
export class ClaudeNormalizer extends BaseNormalizer {
  private jsonBuffer: string = '';
  private currentBlockType: string | null = null;
  private currentBlockIndex: number = -1;
  private pendingToolInputJson: string = '';  // 累积工具输入的增量 JSON

  constructor(config?: Partial<NormalizerConfig>) {
    super({
      agent: 'claude',  // ✅ 使用 agent
      defaultSource: 'worker',
      ...config,
    });
  }

  /**
   * 🔧 覆盖 processTextDelta
   *
   * WorkerLLMAdapter 调用此方法传入文本增量。
   * 对于 Claude，这些文本增量是需要解析的 JSON 事件流。
   * 因此我们将其重定向到 parseChunk 以利用 JSON 解析逻辑。
   */
  processTextDelta(messageId: string, delta: string): void {
    const context = this.getContext(messageId);
    if (!context) {
      this.debug(`[claude] 未找到消息上下文: ${messageId}`);
      return;
    }

    // 🔧 必须更新 rawBuffer，因为 parseChunk 中的 detectInteraction 依赖它
    context.rawBuffer += delta;

    // 调用 parseChunk 进行 JSON 解析（而不是像父类那样直接 append）
    const updates = this.parseChunk(context, delta);

    // 触发更新事件
    for (const update of updates) {
      this.emit(MESSAGE_EVENTS.UPDATE, update);
    }
  }

  /**
   * 获取活跃上下文（暴露给 processTextDelta 使用）
   */
  private getContext(messageId: string): ParseContext | undefined {
    return this.activeContexts.get(messageId);
  }

  protected parseChunk(context: ParseContext, chunk: string): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    
    // 累积 JSON 数据
    this.jsonBuffer += chunk;
    
    // 尝试解析完整的 JSON 行
    const lines = this.jsonBuffer.split('\n');
    this.jsonBuffer = lines.pop() || ''; // 保留不完整的最后一行
    
    for (const line of lines) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      
      try {
        const event = JSON.parse(trimmed) as any;
        // Claude stream-json 可能包裹在 stream_event 中
        if (event?.type === 'stream_event' && event.event) {
          const eventUpdates = this.processEvent(context, event.event as ClaudeStreamEvent);
          updates.push(...eventUpdates);
        } else {
          const eventUpdates = this.processEvent(context, event as ClaudeStreamEvent);
          updates.push(...eventUpdates);
        }
      } catch {
        // 非 JSON 行，作为纯文本处理
        if (trimmed) {
          context.pendingText += trimmed + '\n';
          context.hasAssistantText = true;
          updates.push(this.createUpdate(context.messageId, 'append', { appendText: trimmed + '\n' }));
        }
      }
    }
    
    return updates;
  }

  private processEvent(context: ParseContext, event: ClaudeStreamEvent): StreamUpdate[] {
    const updates: StreamUpdate[] = [];

    if (event.type === 'assistant' && event.message?.content?.length) {
      for (const block of event.message.content) {
        if (block.type === 'text' && block.text) {
          context.pendingText += block.text;
          context.hasAssistantText = true;
          updates.push(this.createUpdate(context.messageId, 'append', { appendText: block.text }));
        }
      }
      return updates;
    }

    if (event.type === 'result') {
      const resultText = typeof event.result === 'string'
        ? event.result
        : (event.result?.content || event.result?.output || '');
      if (resultText) {
        if (!context.hasAssistantText) {
          context.pendingText += resultText;
          updates.push(this.createUpdate(context.messageId, 'append', { appendText: resultText }));
          context.hasAssistantText = true;
        } else if (context.pendingText && resultText.startsWith(context.pendingText)) {
          const tail = resultText.slice(context.pendingText.length);
          if (tail) {
            context.pendingText += tail;
            updates.push(this.createUpdate(context.messageId, 'append', { appendText: tail }));
          }
        } else if (!context.pendingText || !context.pendingText.includes(resultText)) {
          // 追加保护: 只在未包含时追加，避免重复
          context.pendingText += resultText;
          updates.push(this.createUpdate(context.messageId, 'append', { appendText: resultText }));
        }
      }
      return updates;
    }

    switch (event.type) {
      case 'message_start':
        // 消息开始，可以获取消息 ID
        this.debug('[Claude] message_start');
        break;
        
      case 'content_block_start':
        this.currentBlockIndex = event.index ?? -1;
        this.currentBlockType = event.content_block?.type || null;

        if (this.currentBlockType === 'tool_use' && event.content_block) {
          // 工具调用开始 - 清空累积的 JSON，准备接收增量数据
          this.pendingToolInputJson = '';
          const toolCall: ToolCallBlock = {
            type: 'tool_call',
            toolName: event.content_block.name || 'unknown',
            toolId: event.content_block.id || generateMessageId(),
            status: 'running',
            input: undefined,  // 初始时 input 为空，等待增量数据
          };
          this.upsertToolCall(context, toolCall);
          updates.push(this.createUpdate(context.messageId, 'block_update', { blocks: [toolCall] }));
        }
        break;

      case 'content_block_delta':
        if (event.delta) {
          if (event.delta.type === 'text_delta' && event.delta.text) {
            context.pendingText += event.delta.text;
            context.hasAssistantText = true;
            updates.push(this.createUpdate(context.messageId, 'append', { appendText: event.delta.text }));
          } else if (event.delta.type === 'thinking_delta' && event.delta.thinking) {
            if (context.pendingThinking === null) {
              context.pendingThinking = '';
            }
            context.pendingThinking += event.delta.thinking;
            if (!context.thinkingBlockId) {
              context.thinkingBlockId = `${context.messageId}-thinking`;
            }
            updates.push(this.createUpdate(context.messageId, 'block_update', {
              blocks: [{
                type: 'thinking',
                content: context.pendingThinking,
                blockId: context.thinkingBlockId,
              }],
            }));
          } else if (event.delta.type === 'input_json_delta' && event.delta.partial_json) {
            // 累积工具输入的增量 JSON
            this.pendingToolInputJson += event.delta.partial_json;
            this.debug('[Claude] tool input delta:', event.delta.partial_json);
          }
        }
        break;

      case 'content_block_stop':
        // 内容块结束
        if (this.currentBlockType === 'thinking' && context.pendingThinking) {
          this.addThinkingBlock(context, context.pendingThinking, undefined, context.thinkingBlockId);
          context.pendingThinking = null;
        }
        // 工具调用结束 - 使用累积的 JSON 更新工具输入
        if (this.currentBlockType === 'tool_use' && this.pendingToolInputJson) {
          const toolId = this.findActiveToolId(context);
          if (toolId) {
            const toolCall = context.activeToolCalls.get(toolId);
            if (toolCall) {
              try {
                // 解析累积的 JSON 并格式化
                const parsedInput = JSON.parse(this.pendingToolInputJson);
                toolCall.input = JSON.stringify(parsedInput, null, 2);
              } catch {
                // 解析失败，直接使用原始字符串
                toolCall.input = this.pendingToolInputJson;
              }
              // 发送更新
              updates.push(this.createUpdate(context.messageId, 'block_update', { blocks: [toolCall] }));
            }
          }
          this.pendingToolInputJson = '';
        }
        this.currentBlockType = null;
        this.currentBlockIndex = -1;
        break;
        
      case 'tool_result':
        // 工具执行结果
        if (event.result) {
          const toolId = this.findActiveToolId(context);
          if (toolId) {
            if (typeof event.result === 'string') {
              this.completeToolCall(context, toolId, event.result, undefined);
            } else {
              const output = event.result.content || event.result.output || '';
              const error = event.result.is_error ? output : undefined;
              this.completeToolCall(context, toolId, event.result.is_error ? undefined : output, error);
            }
          }
        }
        break;
        
      case 'message_delta':
        // 消息级别的增量更新
        break;
        
      case 'message_stop':
        // 消息结束
        this.debug('[Claude] message_stop');
        break;
        
      case 'error':
        // 错误事件
        this.debug('[Claude] error event:', event);
        break;
    }
    
    // 检测交互请求
    const interaction = this.detectInteraction(context, context.pendingText);
    if (interaction && !context.interaction) {
      context.interaction = interaction;
    }
    
    return updates;
  }

  protected finalizeContext(context: ParseContext): void {
    // 处理剩余的 JSON 缓冲
    if (this.jsonBuffer.trim()) {
      try {
        const event = JSON.parse(this.jsonBuffer) as ClaudeStreamEvent;
        this.processEvent(context, event);
      } catch {
        // 作为纯文本处理
        context.pendingText += this.jsonBuffer;
      }
    }
    this.jsonBuffer = '';
    
    // 处理剩余的思考内容
    if (context.pendingThinking) {
      this.addThinkingBlock(context, context.pendingThinking);
      context.pendingThinking = null;
    }
  }

  protected detectInteraction(context: ParseContext, text: string): InteractionRequest | null {
    // 检测权限请求
    const permissionPatterns = [
      /Do you want to proceed\?/i,
      /Allow .+ to/i,
      /Grant permission/i,
      /\[Y\/n\]/i,
      /\[yes\/no\]/i,
    ];
    
    for (const pattern of permissionPatterns) {
      if (pattern.test(text)) {
        return {
          type: InteractionType.PERMISSION,
          requestId: generateMessageId(),
          prompt: text,
          options: [
            { value: 'yes', label: '是', isDefault: true },
            { value: 'no', label: '否' },
          ],
          required: true,
        };
      }
    }
    
    // 检测确认请求
    const confirmPatterns = [
      /确认|confirm/i,
      /是否继续|continue\?/i,
    ];
    
    for (const pattern of confirmPatterns) {
      if (pattern.test(text)) {
        return {
          type: InteractionType.CLARIFICATION,
          requestId: generateMessageId(),
          prompt: text,
          required: false,
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
