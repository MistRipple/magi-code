/**
 * Token 计数器
 *
 * 支持两种模式：
 * - 精确计算: 使用 tiktoken (GPT-4)
 * - 估算: 字符数 / 4
 */

import { ConfigManager } from '../config';

// 延迟加载 tiktoken，避免启动时的性能开销
let tiktokenEncoder: any = null;

export class TokenCounter {
  /**
   * 获取 tiktoken encoder (延迟加载)
   */
  private static async getEncoder(): Promise<any> {
    if (!tiktokenEncoder) {
      try {
        const tiktoken = await import('tiktoken');
        tiktokenEncoder = tiktoken.encoding_for_model('gpt-4');
      } catch (error) {
        console.warn('Failed to load tiktoken, falling back to estimation:', error);
        return null;
      }
    }
    return tiktokenEncoder;
  }

  /**
   * 计算文本的 Token 数
   */
  static async count(text: string): Promise<number> {
    if (!text) return 0;
    const config = ConfigManager.getInstance().get('performance');

    if (config.enablePreciseTokenCounting) {
      try {
        const encoder = await this.getEncoder();
        if (encoder) {
          const tokens = encoder.encode(text);
          return tokens.length;
        }
      } catch (error) {
        // 切换到估算
      }
    }

    return this.estimate(text);
  }

  /**
   * 同步估算 Token 数 (用于快速计算)
   */
  static estimate(text: string): number {
    return Math.ceil(text.length / 4);
  }

  /**
   * 计算消息列表的 Token 数
   */
  static async countMessages(messages: Array<{ role: string; content: string }>): Promise<number> {
    if (messages.length === 0) return 0;
    const config = ConfigManager.getInstance().get('performance');
    let total = 0;

    if (config.enablePreciseTokenCounting) {
      try {
        const encoder = await this.getEncoder();
        if (encoder) {
          for (const msg of messages) {
            if (msg.content) {
              total += encoder.encode(msg.content).length;
            }
            total += 4; // 消息格式开销 (role, name, etc.)
          }
          total += 3; // 对话开始/结束标记
          return total;
        }
      } catch (error) {
        // 切换到估算
      }
    }

    return this.estimateMessages(messages);
  }

  /**
   * 同步估算消息列表的 Token 数
   */
  static estimateMessages(messages: Array<{ role: string; content: string }>): number {
    let total = 0;

    for (const msg of messages) {
      total += this.estimate(msg.content);
      total += 4;
    }

    total += 3;

    return total;
  }

  /**
   * 批量计算多个文本的 Token 数
   */
  static async countBatch(texts: string[]): Promise<number[]> {
    const config = ConfigManager.getInstance().get('performance');

    if (config.enablePreciseTokenCounting) {
      try {
        const encoder = await this.getEncoder();
        if (encoder) {
          return texts.map(text => (text ? encoder.encode(text).length : 0));
        }
      } catch (error) {
        // 切换到估算
      }
    }

    return texts.map(text => (text ? this.estimate(text) : 0));
  }

  /**
   * 释放 encoder 资源
   */
  static dispose(): void {
    if (tiktokenEncoder) {
      try {
        tiktokenEncoder.free();
      } catch (error) {
        // 忽略错误
      }
      tiktokenEncoder = null;
    }
  }
}
