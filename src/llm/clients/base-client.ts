/**
 * LLM 客户端抽象基类
 */

import { EventEmitter } from 'events';
import { LLMConfig } from '../../types/agent-types';
import {
  LLMClient,
  LLMMessageParams,
  LLMResponse,
  LLMStreamChunk,
} from '../types';
import { logger, LogCategory } from '../../logging';

/**
 * LLM 客户端基类
 */
export abstract class BaseLLMClient extends EventEmitter implements LLMClient {
  public readonly config: LLMConfig;

  constructor(config: LLMConfig) {
    super();
    this.config = config;
  }

  /**
   * 发送消息（非流式）
   */
  abstract sendMessage(params: LLMMessageParams): Promise<LLMResponse>;

  /**
   * 发送消息（流式）
   */
  abstract streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse>;

  /**
   * 测试连接
   */
  async testConnection(): Promise<boolean> {
    try {
      const response = await this.sendMessage({
        messages: [{ role: 'user', content: 'test' }],
        maxTokens: 10,
      });
      return !!response;
    } catch (error) {
      logger.error('Connection test failed', { error }, LogCategory.LLM);
      return false;
    }
  }

  /**
   * 验证配置
   */
  protected validateConfig(): void {
    if (!this.config.apiKey) {
      throw new Error(`API key is required for ${this.config.provider}`);
    }
    if (!this.config.model) {
      throw new Error(`Model is required for ${this.config.provider}`);
    }
    if (!this.config.baseUrl) {
      throw new Error(`Base URL is required for ${this.config.provider}`);
    }
  }

  /**
   * 记录请求
   */
  protected logRequest(params: LLMMessageParams): void {
    logger.debug('Sending LLM request', {
      provider: this.config.provider,
      model: this.config.model,
      messageCount: params.messages.length,
      hasTools: !!params.tools?.length,
      stream: params.stream,
    }, LogCategory.LLM);
  }

  /**
   * 记录响应
   */
  protected logResponse(response: LLMResponse): void {
    logger.debug('Received LLM response', {
      provider: this.config.provider,
      model: this.config.model,
      contentLength: response.content.length,
      toolCalls: response.toolCalls?.length || 0,
      usage: response.usage,
      stopReason: response.stopReason,
    }, LogCategory.LLM);
  }

  /**
   * 记录错误
   */
  protected logError(error: any, context: string): void {
    logger.error(`LLM error: ${context}`, {
      provider: this.config.provider,
      model: this.config.model,
      error: error.message || error,
      stack: error.stack,
    }, LogCategory.LLM);
  }
}
