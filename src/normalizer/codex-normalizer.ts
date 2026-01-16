/**
 * Codex CLI Normalizer
 * 
 * 解析 Codex CLI 的纯文本/Markdown 格式输出
 * Codex 不支持 stream-json，输出相对简单
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

/**
 * Codex CLI Normalizer
 */
export class CodexNormalizer extends BaseNormalizer {
  private codeBlockBuffer: string = '';
  private inCodeBlock: boolean = false;
  private codeBlockLang: string = '';

  constructor(config?: Partial<NormalizerConfig>) {
    super({
      cli: 'codex',
      defaultSource: 'worker',
      ...config,
    });
  }

  protected parseChunk(context: ParseContext, chunk: string): StreamUpdate[] {
    const updates: StreamUpdate[] = [];
    
    // Codex 输出是纯文本，逐行处理
    const lines = (context.pendingText + chunk).split('\n');
    context.pendingText = '';
    
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      const isLastLine = i === lines.length - 1;
      
      // 检测代码块开始
      const codeBlockStart = line.match(/^```(\w*)/);
      if (codeBlockStart && !this.inCodeBlock) {
        this.inCodeBlock = true;
        this.codeBlockLang = codeBlockStart[1] || 'text';
        this.codeBlockBuffer = '';
        continue;
      }
      
      // 检测代码块结束
      if (line.trim() === '```' && this.inCodeBlock) {
        this.inCodeBlock = false;
        if (this.codeBlockBuffer.trim()) {
          const codeBlock: CodeBlock = {
            type: 'code',
            language: this.codeBlockLang,
            content: this.codeBlockBuffer.trim(),
          };
          context.blocks.push(codeBlock);
          updates.push(this.createUpdate(context.messageId, 'block_update', { blocks: [codeBlock] }));
        }
        this.codeBlockBuffer = '';
        this.codeBlockLang = '';
        continue;
      }
      
      // 在代码块内
      if (this.inCodeBlock) {
        this.codeBlockBuffer += line + '\n';
        continue;
      }
      
      // 普通文本
      if (isLastLine) {
        // 最后一行可能不完整，保留
        context.pendingText = line;
      } else {
        context.pendingText += line + '\n';
        updates.push(this.createUpdate(context.messageId, 'append', { appendText: line + '\n' }));
      }
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