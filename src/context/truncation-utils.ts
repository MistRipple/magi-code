/**
 * TruncationUtils - Augment 风格的截断工具
 * 预防性截断策略，从源头控制上下文大小
 */

import { TruncationConfig, DEFAULT_TRUNCATION_CONFIG } from './types';

/**
 * 截断结果
 */
export interface TruncationResult {
  content: string;
  wasTruncated: boolean;
  originalLength: number;
  truncatedLength: number;
}

/**
 * 截断工具类
 */
export class TruncationUtils {
  private config: TruncationConfig;

  constructor(config: Partial<TruncationConfig> = {}) {
    this.config = { ...DEFAULT_TRUNCATION_CONFIG, ...config };
  }

  /**
   * 截断消息内容（Augment 风格）
   * 直接截断 + 添加提示，不使用 LLM
   */
  truncateMessage(content: string, maxChars?: number): TruncationResult {
    const limit = maxChars ?? this.config.maxMessageChars;
    const originalLength = content.length;

    if (!this.config.enabled || originalLength <= limit) {
      return {
        content,
        wasTruncated: false,
        originalLength,
        truncatedLength: originalLength
      };
    }

    // 智能截断：尝试在句子或段落边界截断
    let truncateAt = limit;
    const searchStart = Math.max(0, limit - 500);
    
    // 优先在段落边界截断
    const paragraphBreak = content.lastIndexOf('\n\n', limit);
    if (paragraphBreak > searchStart) {
      truncateAt = paragraphBreak;
    } else {
      // 其次在句子边界截断
      const sentenceBreak = content.lastIndexOf('. ', limit);
      if (sentenceBreak > searchStart) {
        truncateAt = sentenceBreak + 1;
      } else {
        // 最后在换行符截断
        const lineBreak = content.lastIndexOf('\n', limit);
        if (lineBreak > searchStart) {
          truncateAt = lineBreak;
        }
      }
    }

    const truncatedContent = content.slice(0, truncateAt) + '\n\n' + this.config.truncationNotice;

    return {
      content: truncatedContent,
      wasTruncated: true,
      originalLength,
      truncatedLength: truncatedContent.length
    };
  }

  /**
   * 截断工具输出
   */
  truncateToolOutput(output: string, maxChars?: number): TruncationResult {
    const limit = maxChars ?? this.config.maxToolOutputChars;
    return this.truncateMessage(output, limit);
  }

  /**
   * 截断代码块（保留结构完整性）
   */
  truncateCodeBlock(code: string, maxLines: number = 150): TruncationResult {
    const lines = code.split('\n');
    const originalLength = code.length;

    if (lines.length <= maxLines) {
      return {
        content: code,
        wasTruncated: false,
        originalLength,
        truncatedLength: originalLength
      };
    }

    // 保留开头和结尾的代码
    const headLines = Math.floor(maxLines * 0.6);
    const tailLines = maxLines - headLines - 3; // 3 行用于省略提示

    const head = lines.slice(0, headLines);
    const tail = lines.slice(-tailLines);
    const omittedCount = lines.length - headLines - tailLines;

    const truncatedContent = [
      ...head,
      '',
      `// ... ${omittedCount} lines omitted ...`,
      '',
      ...tail
    ].join('\n');

    return {
      content: truncatedContent,
      wasTruncated: true,
      originalLength,
      truncatedLength: truncatedContent.length
    };
  }

  /**
   * 批量截断消息列表
   */
  truncateMessageList(
    messages: Array<{ role: string; content: string }>,
    totalMaxChars: number
  ): Array<{ role: string; content: string; wasTruncated: boolean }> {
    let remainingChars = totalMaxChars;
    const result: Array<{ role: string; content: string; wasTruncated: boolean }> = [];

    // 从最新的消息开始处理（保留最近的上下文）
    for (let i = messages.length - 1; i >= 0 && remainingChars > 0; i--) {
      const msg = messages[i];
      const truncated = this.truncateMessage(msg.content, remainingChars);
      
      result.unshift({
        role: msg.role,
        content: truncated.content,
        wasTruncated: truncated.wasTruncated
      });

      remainingChars -= truncated.truncatedLength;
    }

    return result;
  }

  /**
   * 更新配置
   */
  updateConfig(config: Partial<TruncationConfig>): void {
    this.config = { ...this.config, ...config };
  }

  /**
   * 获取当前配置
   */
  getConfig(): TruncationConfig {
    return { ...this.config };
  }
}

// 导出单例实例
export const truncationUtils = new TruncationUtils();

