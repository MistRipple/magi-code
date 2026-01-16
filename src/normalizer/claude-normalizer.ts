/**
 * Claude CLI Normalizer
 * 
 * 解析 Claude CLI 的 stream-json 格式输出
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
  result?: {
    type: string;
    content?: string;
    output?: string;
    is_error?: boolean;
  };
}

/**
 * Claude CLI Normalizer
 */
export class ClaudeNormalizer extends BaseNormalizer {
  private jsonBuffer: string = '';
  private currentBlockType: string | null = null;
  private currentBlockIndex: number = -1;

  constructor(config?: Partial<NormalizerConfig>) {
    super({
      cli: 'claude',
      defaultSource: 'worker',
      ...config,
    });
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
        const event = JSON.parse(trimmed) as ClaudeStreamEvent;
        const eventUpdates = this.processEvent(context, event);
        updates.push(...eventUpdates);
      } catch {
        // 非 JSON 行，作为纯文本处理
        if (trimmed) {
          context.pendingText += trimmed + '\n';
          updates.push(this.createUpdate(context.messageId, 'append', { appendText: trimmed + '\n' }));
        }
      }
    }
    
    return updates;
  }

  private processEvent(context: ParseContext, event: ClaudeStreamEvent): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    
    switch (event.type) {
      case 'message_start':
        // 消息开始，可以获取消息 ID
        this.debug('[Claude] message_start');
        break;
        
      case 'content_block_start':
        this.currentBlockIndex = event.index ?? -1;
        this.currentBlockType = event.content_block?.type || null;
        
        if (this.currentBlockType === 'tool_use' && event.content_block) {
          // 工具调用开始
          const toolCall: ToolCallBlock = {
            type: 'tool_call',
            toolName: event.content_block.name || 'unknown',
            toolId: event.content_block.id || generateMessageId(),
            status: 'running',
            input: event.content_block.input,
          };
          this.upsertToolCall(context, toolCall);
          updates.push(this.createUpdate(context.messageId, 'block_update', { blocks: [toolCall] }));
        }
        break;
        
      case 'content_block_delta':
        if (event.delta) {
          if (event.delta.type === 'text_delta' && event.delta.text) {
            context.pendingText += event.delta.text;
            updates.push(this.createUpdate(context.messageId, 'append', { appendText: event.delta.text }));
          } else if (event.delta.type === 'thinking_delta' && event.delta.thinking) {
            if (context.pendingThinking === null) {
              context.pendingThinking = '';
            }
            context.pendingThinking += event.delta.thinking;
          } else if (event.delta.type === 'input_json_delta' && event.delta.partial_json) {
            // 工具输入的增量 JSON
            this.debug('[Claude] tool input delta:', event.delta.partial_json);
          }
        }
        break;
        
      case 'content_block_stop':
        // 内容块结束
        if (this.currentBlockType === 'thinking' && context.pendingThinking) {
          this.addThinkingBlock(context, context.pendingThinking);
          context.pendingThinking = null;
        }
        this.currentBlockType = null;
        this.currentBlockIndex = -1;
        break;
        
      case 'tool_result':
        // 工具执行结果
        if (event.result) {
          const toolId = this.findActiveToolId(context);
          if (toolId) {
            const output = event.result.content || event.result.output || '';
            const error = event.result.is_error ? output : undefined;
            this.completeToolCall(context, toolId, event.result.is_error ? undefined : output, error);
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